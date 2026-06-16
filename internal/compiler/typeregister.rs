// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

// cSpell: ignore imum noarg strarg

use smol_str::{SmolStr, StrExt, ToSmolStr};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use crate::expression_tree::BuiltinFunction;
use crate::langtype::{
    BuiltinElement, BuiltinPropertyDefault, BuiltinPropertyInfo, BuiltinStruct, DefaultSizeBinding,
    ElementType, Enumeration, Function, NativeClass, PropertyLookupResult, Struct, StructName,
    Type,
};
use crate::object_tree::{Component, Element, PropertyDeclaration, PropertyVisibility};
use crate::typeloader;

pub const RESERVED_GEOMETRY_PROPERTIES: &[(&str, Type)] = &[
    ("x", Type::LogicalLength),
    ("y", Type::LogicalLength),
    ("width", Type::LogicalLength),
    ("height", Type::LogicalLength),
    ("z", Type::Float32),
];

pub const RESERVED_LAYOUT_PROPERTIES: &[(&str, Type)] = &[
    ("min-width", Type::LogicalLength),
    ("min-height", Type::LogicalLength),
    ("max-width", Type::LogicalLength),
    ("max-height", Type::LogicalLength),
    ("padding", Type::LogicalLength),
    ("padding-left", Type::LogicalLength),
    ("padding-right", Type::LogicalLength),
    ("padding-top", Type::LogicalLength),
    ("padding-bottom", Type::LogicalLength),
    ("preferred-width", Type::LogicalLength),
    ("preferred-height", Type::LogicalLength),
    ("horizontal-stretch", Type::Float32),
    ("vertical-stretch", Type::Float32),
];

pub const RESERVED_GRIDLAYOUT_PROPERTIES: &[(&str, Type)] = &[
    ("col", Type::Int32),
    ("row", Type::Int32),
    ("colspan", Type::Int32),
    ("rowspan", Type::Int32),
];

// Note: flex-align-self is also a flexbox property but is added in reserved_properties()
// because Type::Enumeration requires a runtime Rc allocation.
pub const RESERVED_FLEXBOXLAYOUT_PROPERTIES: &[(&str, Type)] = &[
    ("flex-grow", Type::Float32),
    ("flex-shrink", Type::Float32),
    ("flex-basis", Type::LogicalLength),
    ("flex-order", Type::Int32),
];

macro_rules! declare_enums {
    ($( $(#[$enum_doc:meta])* $vis:vis enum $Name:ident { $( $(#[$value_doc:meta])* $Value:ident,)* })*) => {
        #[allow(non_snake_case)]
        pub struct BuiltinEnums {
            $(pub $Name : Rc<Enumeration>),*
        }
        impl BuiltinEnums {
            fn new() -> Self {
                Self {
                    $($Name : Rc::new(Enumeration {
                        name: stringify!($Name).replace_smolstr("_", "-"),
                        values: vec![$(crate::generator::to_kebab_case(stringify!($Value).trim_start_matches("r#")).into()),*],
                        default_value: 0,
                        node: None,
                    })),*
                }
            }
            fn fill_register(&self, register: &mut TypeRegister) {
                $(if stringify!($Name) != "PathEvent" {
                    register.insert_type_with_name(
                        Type::Enumeration(self.$Name.clone()),
                        stringify!($Name).replace_smolstr("_", "-")
                    );
                })*
            }
        }
    };
}

i_slint_common::for_each_enums!(declare_enums);

pub struct BuiltinTypes {
    pub enums: BuiltinEnums,
    pub noarg_callback_type: Type,
    pub strarg_callback_type: Type,
    pub logical_point_type: Rc<Struct>,
    pub logical_size_type: Rc<Struct>,
    pub font_metrics_type: Type,
    pub layout_info_type: Rc<Struct>,
    pub state_info_type: Rc<Struct>,
    pub gridlayout_input_data_type: Type,
    pub path_element_type: Type,
    pub layout_item_info_type: Type,
    pub flexbox_layout_item_info_type: Type,
}

impl BuiltinTypes {
    fn new() -> Self {
        let layout_info_type = Rc::new(Struct {
            fields: ["min", "max", "preferred"]
                .iter()
                .map(|s| (SmolStr::new_static(s), Type::LogicalLength))
                .chain(
                    ["min_percent", "max_percent", "stretch"]
                        .iter()
                        .map(|s| (SmolStr::new_static(s), Type::Float32)),
                )
                .collect(),
            name: BuiltinStruct::LayoutInfo.into(),
        });
        let enums = BuiltinEnums::new();
        let flex_align_self_type = Type::Enumeration(enums.FlexboxLayoutAlignSelf.clone());
        Self {
            enums,
            logical_point_type: Rc::new(Struct {
                fields: IntoIterator::into_iter([
                    (SmolStr::new_static("x"), Type::LogicalLength),
                    (SmolStr::new_static("y"), Type::LogicalLength),
                ])
                .collect(),
                name: BuiltinStruct::LogicalPosition.into(),
            }),
            logical_size_type: Rc::new(Struct {
                fields: IntoIterator::into_iter([
                    (SmolStr::new_static("width"), Type::LogicalLength),
                    (SmolStr::new_static("height"), Type::LogicalLength),
                ])
                .collect(),
                name: BuiltinStruct::LogicalSize.into(),
            }),
            font_metrics_type: Type::Struct(Rc::new(Struct {
                fields: IntoIterator::into_iter([
                    (SmolStr::new_static("ascent"), Type::LogicalLength),
                    (SmolStr::new_static("descent"), Type::LogicalLength),
                    (SmolStr::new_static("x-height"), Type::LogicalLength),
                    (SmolStr::new_static("cap-height"), Type::LogicalLength),
                ])
                .collect(),
                name: BuiltinStruct::FontMetrics.into(),
            })),
            noarg_callback_type: Type::Callback(Rc::new(Function {
                return_type: Type::Void,
                args: Vec::new(),
                arg_names: Vec::new(),
            })),
            strarg_callback_type: Type::Callback(Rc::new(Function {
                return_type: Type::Void,
                args: vec![Type::String],
                arg_names: Vec::new(),
            })),
            layout_info_type: layout_info_type.clone(),
            state_info_type: Rc::new(Struct {
                fields: IntoIterator::into_iter([
                    (SmolStr::new_static("current-state"), Type::Int32),
                    (SmolStr::new_static("previous-state"), Type::Int32),
                    (SmolStr::new_static("change-time"), Type::Duration),
                ])
                .collect(),
                name: BuiltinStruct::StateInfo.into(),
            }),
            path_element_type: Type::Struct(Rc::new(Struct {
                fields: Default::default(),
                name: BuiltinStruct::PathElement.into(),
            })),
            layout_item_info_type: Type::Struct(Rc::new(Struct {
                fields: IntoIterator::into_iter([(
                    "constraint".into(),
                    layout_info_type.clone().into(),
                )])
                .collect(),
                name: BuiltinStruct::LayoutItemInfo.into(),
            })),
            flexbox_layout_item_info_type: Type::Struct(Rc::new(Struct {
                fields: IntoIterator::into_iter([
                    ("constraint".into(), layout_info_type.into()),
                    ("flex-grow".into(), Type::Float32),
                    ("flex-shrink".into(), Type::Float32),
                    ("flex-basis".into(), Type::Float32),
                    ("flex-align-self".into(), flex_align_self_type),
                    ("flex-order".into(), Type::Int32),
                ])
                .collect(),
                name: BuiltinStruct::FlexboxLayoutItemInfo.into(),
            })),
            gridlayout_input_data_type: Type::Struct(Rc::new(Struct {
                fields: IntoIterator::into_iter([
                    ("row".into(), Type::Int32),
                    ("column".into(), Type::Int32),
                    ("rowspan".into(), Type::Int32),
                    ("colspan".into(), Type::Int32),
                ])
                .collect(),
                name: BuiltinStruct::GridLayoutInputData.into(),
            })),
        }
    }
}

thread_local! {
    pub static BUILTIN: BuiltinTypes = BuiltinTypes::new();
}

const RESERVED_OTHER_PROPERTIES: &[(&str, Type)] = &[
    ("clip", Type::Bool),
    ("opacity", Type::Float32),
    ("cache-rendering-hint", Type::Bool),
    ("visible", Type::Bool), // ("enabled", Type::Bool),
];

pub const RESERVED_DROP_SHADOW_PROPERTIES: &[(&str, Type)] = &[
    ("drop-shadow-offset-x", Type::LogicalLength),
    ("drop-shadow-offset-y", Type::LogicalLength),
    ("drop-shadow-blur", Type::LogicalLength),
    ("drop-shadow-spread", Type::LogicalLength),
    ("drop-shadow-color", Type::Color),
];

pub const RESERVED_INSET_SHADOW_PROPERTIES: &[(&str, Type)] = &[
    ("inset-shadow-offset-x", Type::LogicalLength),
    ("inset-shadow-offset-y", Type::LogicalLength),
    ("inset-shadow-blur", Type::LogicalLength),
    ("inset-shadow-spread", Type::LogicalLength),
    ("inset-shadow-color", Type::Color),
];

pub const RESERVED_TRANSFORM_PROPERTIES: &[(&str, Type)] = &[
    ("transform-rotation", Type::Angle),
    ("transform-scale-x", Type::Float32),
    ("transform-scale-y", Type::Float32),
    ("transform-scale", Type::Float32),
];

pub fn transform_origin_property() -> (&'static str, Rc<Struct>) {
    ("transform-origin", logical_point_type())
}

pub const DEPRECATED_ROTATION_ORIGIN_PROPERTIES: [(&str, Type); 2] =
    [("rotation-origin-x", Type::LogicalLength), ("rotation-origin-y", Type::LogicalLength)];

pub fn noarg_callback_type() -> Type {
    BUILTIN.with(|types| types.noarg_callback_type.clone())
}

fn strarg_callback_type() -> Type {
    BUILTIN.with(|types| types.strarg_callback_type.clone())
}

pub fn reserved_accessibility_properties() -> impl Iterator<Item = (&'static str, Type)> {
    [
        //("accessible-role", ...)
        ("accessible-checkable", Type::Bool),
        ("accessible-checked", Type::Bool),
        ("accessible-delegate-focus", Type::Int32),
        ("accessible-description", Type::String),
        ("accessible-enabled", Type::Bool),
        ("accessible-expandable", Type::Bool),
        ("accessible-expanded", Type::Bool),
        ("accessible-id", Type::String),
        ("accessible-label", Type::String),
        ("accessible-value", Type::String),
        ("accessible-value-maximum", Type::Float32),
        ("accessible-value-minimum", Type::Float32),
        ("accessible-value-step", Type::Float32),
        ("accessible-placeholder-text", Type::String),
        ("accessible-action-default", noarg_callback_type()),
        ("accessible-action-increment", noarg_callback_type()),
        ("accessible-action-decrement", noarg_callback_type()),
        ("accessible-action-set-value", strarg_callback_type()),
        ("accessible-action-expand", noarg_callback_type()),
        ("accessible-item-selectable", Type::Bool),
        ("accessible-item-selected", Type::Bool),
        ("accessible-item-index", Type::Int32),
        ("accessible-item-count", Type::Int32),
        ("accessible-read-only", Type::Bool),
    ]
    .into_iter()
}

/// list of reserved property injected in every item
pub fn reserved_properties() -> impl Iterator<Item = (&'static str, Type, PropertyVisibility)> {
    RESERVED_GEOMETRY_PROPERTIES
        .iter()
        .chain(RESERVED_LAYOUT_PROPERTIES.iter())
        .chain(RESERVED_OTHER_PROPERTIES.iter())
        .chain(RESERVED_DROP_SHADOW_PROPERTIES.iter())
        .chain(RESERVED_INSET_SHADOW_PROPERTIES.iter())
        .chain(RESERVED_TRANSFORM_PROPERTIES.iter())
        .chain(DEPRECATED_ROTATION_ORIGIN_PROPERTIES.iter())
        .map(|(k, v)| (*k, v.clone(), PropertyVisibility::Input))
        .chain(
            std::iter::once(transform_origin_property())
                .map(|(k, v)| (k, v.into(), PropertyVisibility::Input)),
        )
        .chain(reserved_accessibility_properties().map(|(k, v)| (k, v, PropertyVisibility::Input)))
        .chain(
            RESERVED_GRIDLAYOUT_PROPERTIES
                .iter()
                .map(|(k, v)| (*k, v.clone(), PropertyVisibility::Input)),
        )
        .chain(
            RESERVED_FLEXBOXLAYOUT_PROPERTIES
                .iter()
                .map(|(k, v)| (*k, v.clone(), PropertyVisibility::Input)),
        )
        // flex-align-self is a flexbox-layout property but can't be in the const array
        // because Type::Enumeration requires a runtime Rc allocation.
        .chain(std::iter::once((
            "flex-align-self",
            Type::Enumeration(BUILTIN.with(|e| e.enums.FlexboxLayoutAlignSelf.clone())),
            PropertyVisibility::Input,
        )))
        .chain(IntoIterator::into_iter([
            ("absolute-position", logical_point_type().into(), PropertyVisibility::Output),
            ("forward-focus", Type::ElementReference, PropertyVisibility::Constexpr),
            (
                "focus",
                Type::Function(BuiltinFunction::SetFocusItem.ty()),
                PropertyVisibility::Public,
            ),
            (
                "clear-focus",
                Type::Function(BuiltinFunction::ClearFocusItem.ty()),
                PropertyVisibility::Public,
            ),
            (
                "dialog-button-role",
                Type::Enumeration(BUILTIN.with(|e| e.enums.DialogButtonRole.clone())),
                PropertyVisibility::Constexpr,
            ),
            (
                "accessible-role",
                Type::Enumeration(BUILTIN.with(|e| e.enums.AccessibleRole.clone())),
                PropertyVisibility::Constexpr,
            ),
            (
                "accessible-orientation",
                Type::Enumeration(BUILTIN.with(|e| e.enums.Orientation.clone())),
                PropertyVisibility::Input,
            ),
            (
                "accessible-live-region",
                Type::Enumeration(BUILTIN.with(|e| e.enums.AccessibleLiveRegion.clone())),
                PropertyVisibility::Input,
            ),
        ]))
        .chain(std::iter::once(("init", noarg_callback_type(), PropertyVisibility::Private)))
}

/// lookup reserved property injected in every item
pub fn reserved_property(name: std::borrow::Cow<'_, str>) -> PropertyLookupResult<'_> {
    thread_local! {
        static RESERVED_PROPERTIES: HashMap<&'static str, (Type, PropertyVisibility, Option<BuiltinFunction>)>
            = reserved_properties().map(|(name, ty, visibility)| (name, (ty, visibility, reserved_member_function(name)))).collect();
    }
    if let Some((ty, visibility, builtin_function)) =
        RESERVED_PROPERTIES.with(|reserved| reserved.get(name.as_ref()).cloned())
    {
        return PropertyLookupResult {
            property_type: ty,
            resolved_name: name,
            is_local_to_component: false,
            is_in_direct_base: false,
            property_visibility: visibility,
            declared_pure: None,
            builtin_function,
        };
    }

    // Report deprecated known reserved properties (maximum_width, minimum_height, ...)
    for pre in &["min", "max"] {
        if let Some(a) = name.strip_prefix(pre) {
            for suf in &["width", "height"] {
                if let Some(b) = a.strip_suffix(suf)
                    && b == "imum-"
                {
                    return PropertyLookupResult {
                        property_type: Type::LogicalLength,
                        resolved_name: format!("{pre}-{suf}").into(),
                        is_local_to_component: false,
                        is_in_direct_base: false,
                        property_visibility: crate::object_tree::PropertyVisibility::InOut,
                        declared_pure: None,
                        builtin_function: None,
                    };
                }
            }
        }
    }
    PropertyLookupResult::invalid(name)
}

/// These member functions are injected in every time
pub fn reserved_member_function(name: &str) -> Option<BuiltinFunction> {
    for (m, e) in [
        ("focus", BuiltinFunction::SetFocusItem), // match for callable "focus" property
        ("clear-focus", BuiltinFunction::ClearFocusItem), // match for callable "clear-focus" property
    ] {
        if m == name {
            return Some(e);
        }
    }
    None
}

/// All types (datatypes, internal elements, properties, ...) are stored in this type
#[derive(Debug, Default)]
pub struct TypeRegister {
    /// The set of property types.
    types: HashMap<SmolStr, Type>,
    /// The set of element types
    elements: HashMap<SmolStr, ElementType>,
    supported_property_animation_types: HashSet<String>,
    pub(crate) property_animation_type: ElementType,
    pub(crate) empty_type: ElementType,
    /// Map from a context restricted type to the list of contexts (parent type) it is allowed in. This is
    /// used to construct helpful error messages, such as "Row can only be within a GridLayout element".
    context_restricted_types: HashMap<SmolStr, HashSet<SmolStr>>,
    parent_registry: Option<Rc<RefCell<TypeRegister>>>,
    /// If the lookup function should return types that are marked as internal
    pub(crate) expose_internal_types: bool,
}

impl TypeRegister {
    pub(crate) fn snapshot(&self, snapshotter: &mut typeloader::Snapshotter) -> Self {
        Self {
            types: self.types.clone(),
            elements: self
                .elements
                .iter()
                .map(|(k, v)| (k.clone(), snapshotter.snapshot_element_type(v)))
                .collect(),
            supported_property_animation_types: self.supported_property_animation_types.clone(),
            property_animation_type: snapshotter
                .snapshot_element_type(&self.property_animation_type),
            empty_type: snapshotter.snapshot_element_type(&self.empty_type),
            context_restricted_types: self.context_restricted_types.clone(),
            parent_registry: self
                .parent_registry
                .as_ref()
                .map(|tr| snapshotter.snapshot_type_register(tr)),
            expose_internal_types: self.expose_internal_types,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn freeze_builtin_registry_metadata(
        &self,
    ) -> crate::frozen_builtins::FrozenBuiltinRegistry {
        let mut types = self.types.keys().map(ToString::to_string).collect::<Vec<_>>();
        types.sort();
        let mut structs = self
            .types
            .iter()
            .filter_map(|(name, ty)| {
                let Type::Struct(struct_ty) = ty else {
                    return None;
                };
                Some(crate::frozen_builtins::FrozenBuiltinStruct {
                    name: name.to_string(),
                    fields: struct_ty
                        .fields
                        .iter()
                        .map(|(name, ty)| crate::frozen_builtins::FrozenBuiltinStructField {
                            name: name.to_string(),
                            ty: ty.to_string(),
                        })
                        .collect(),
                })
            })
            .collect::<Vec<_>>();
        structs.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

        let mut elements = self
            .elements
            .iter()
            .map(|(name, element_type)| match element_type {
                ElementType::Builtin(builtin) => {
                    freeze_builtin_registry_element(name.as_str(), "builtin", builtin)
                }
                ElementType::Component(component) => {
                    freeze_component_registry_element(name.as_str(), "component", component)
                }
                _ => crate::frozen_builtins::FrozenBuiltinRegistryElement {
                    name: name.to_string(),
                    kind: element_type_kind(element_type).into(),
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();
        let mut seen_elements =
            elements.iter().map(|element| element.name.clone()).collect::<HashSet<_>>();
        let mut child_stack = self
            .elements
            .values()
            .filter_map(|element_type| match element_type {
                ElementType::Builtin(builtin) => Some(builtin),
                _ => None,
            })
            .flat_map(|builtin| builtin.additional_accepted_child_types.values().cloned())
            .collect::<Vec<_>>();
        while let Some(child) = child_stack.pop() {
            if !seen_elements.insert(child.name.to_string()) {
                continue;
            }
            child_stack.extend(child.additional_accepted_child_types.values().cloned());
            elements.push(freeze_builtin_registry_element(child.name.as_str(), "builtin", &child));
        }
        elements.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

        let mut supported_property_animation_types =
            self.supported_property_animation_types.iter().cloned().collect::<Vec<_>>();
        supported_property_animation_types.sort();

        let mut context_restricted_types = self
            .context_restricted_types
            .iter()
            .map(|(name, contexts)| {
                let mut contexts = contexts.iter().map(ToString::to_string).collect::<Vec<_>>();
                contexts.sort();
                crate::frozen_builtins::FrozenBuiltinContextRestriction {
                    name: name.to_string(),
                    contexts,
                }
            })
            .collect::<Vec<_>>();
        context_restricted_types.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

        crate::frozen_builtins::FrozenBuiltinRegistry {
            types,
            structs,
            elements,
            supported_property_animation_types,
            property_animation_type: self.property_animation_type.to_string(),
            property_animation_element: match &self.property_animation_type {
                ElementType::Builtin(builtin) => {
                    Some(freeze_builtin_registry_element(builtin.name.as_str(), "builtin", builtin))
                }
                _ => None,
            },
            empty_type: self.empty_type.to_string(),
            empty_element: match &self.empty_type {
                ElementType::Builtin(builtin) => {
                    Some(freeze_builtin_registry_element(builtin.name.as_str(), "builtin", builtin))
                }
                _ => None,
            },
            context_restricted_types,
            expose_internal_types: self.expose_internal_types,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn rehydrate_builtin_registry_shell(
        frozen: &crate::frozen_builtins::FrozenBuiltinRegistry,
    ) -> Rc<RefCell<Self>> {
        let mut registry = TypeRegister {
            supported_property_animation_types: frozen
                .supported_property_animation_types
                .iter()
                .cloned()
                .collect(),
            context_restricted_types: frozen
                .context_restricted_types
                .iter()
                .map(|restriction| {
                    (
                        SmolStr::new(restriction.name.as_str()),
                        restriction.contexts.iter().map(|ctx| SmolStr::new(ctx.as_str())).collect(),
                    )
                })
                .collect(),
            expose_internal_types: frozen.expose_internal_types,
            ..Default::default()
        };

        for ty_name in &frozen.types {
            if let Some(ty) = builtin_type_from_name(ty_name) {
                registry.insert_type_with_name(ty, SmolStr::new(ty_name.as_str()));
            }
        }
        BUILTIN.with(|e| e.enums.fill_register(&mut registry));
        for frozen_struct in &frozen.structs {
            let fields = frozen_struct
                .fields
                .iter()
                .map(|field| {
                    (
                        SmolStr::new(field.name.as_str()),
                        rehydrate_registry_property_type(&field.ty, &registry),
                    )
                })
                .collect();
            let name = frozen_struct
                .name
                .parse::<BuiltinStruct>()
                .map(StructName::Builtin)
                .unwrap_or(StructName::None);
            registry.insert_type_with_name(
                Type::Struct(Rc::new(Struct { fields, name })),
                SmolStr::new(frozen_struct.name.as_str()),
            );
        }

        for frozen_element in &frozen.elements {
            if frozen_element.kind != "builtin" {
                continue;
            }
            registry.add_builtin(Rc::new(rehydrate_builtin_registry_element(
                frozen_element,
                &registry,
            )));
        }

        for frozen_element in &frozen.elements {
            match frozen_element.kind.as_str() {
                "component" => {
                    let base_type =
                        rehydrate_registry_component_root_base_type(&registry, frozen_element);
                    let root_element = Element {
                        id: SmolStr::new(frozen_element.component_root_id.as_str()),
                        base_type,
                        property_declarations: rehydrate_root_property_declarations(
                            &frozen_element.component_root_properties,
                            &registry,
                        ),
                        ..Default::default()
                    }
                    .make_rc();
                    let component = Rc::new(Component {
                        id: SmolStr::new(frozen_element.name.as_str()),
                        root_element,
                        ..Default::default()
                    });
                    component.root_element.borrow_mut().enclosing_component =
                        Rc::downgrade(&component);
                    registry.add_with_name(SmolStr::new(frozen_element.name.as_str()), component);
                }
                "global" => {
                    registry
                        .elements
                        .insert(SmolStr::new(frozen_element.name.as_str()), ElementType::Global);
                }
                "interface" => {
                    registry
                        .elements
                        .insert(SmolStr::new(frozen_element.name.as_str()), ElementType::Interface);
                }
                _ => {}
            }
        }

        let frozen_elements_by_name = frozen
            .elements
            .iter()
            .map(|element| (element.name.as_str(), element))
            .collect::<HashMap<_, _>>();

        for frozen_element in &frozen.elements {
            if frozen_element.kind != "builtin" || frozen_element.accepted_child_types.is_empty() {
                continue;
            }

            let child_types = frozen_element
                .accepted_child_types
                .iter()
                .filter_map(|child_name| {
                    Some((
                        SmolStr::new(child_name.as_str()),
                        rehydrate_accepted_child_builtin(
                            child_name,
                            &frozen_elements_by_name,
                            &registry,
                        )?,
                    ))
                })
                .collect::<HashMap<_, _>>();

            let Some(ElementType::Builtin(parent)) =
                registry.elements.get_mut(frozen_element.name.as_str())
            else {
                continue;
            };
            Rc::make_mut(parent).additional_accepted_child_types = child_types;
        }

        registry.property_animation_type = frozen
            .property_animation_element
            .as_ref()
            .map(|element| {
                ElementType::Builtin(Rc::new(rehydrate_builtin_registry_element(
                    element, &registry,
                )))
            })
            .unwrap_or_else(|| {
                rehydrate_registry_element_reference(&registry, &frozen.property_animation_type)
            });
        registry.empty_type = frozen
            .empty_element
            .as_ref()
            .map(|element| {
                ElementType::Builtin(Rc::new(rehydrate_builtin_registry_element(
                    element, &registry,
                )))
            })
            .unwrap_or_else(|| rehydrate_registry_element_reference(&registry, &frozen.empty_type));

        Rc::new(RefCell::new(registry))
    }

    /// Insert a type into the type register with its builtin type name.
    ///
    /// Returns false if it replaced an existing type.
    pub fn insert_type(&mut self, t: Type) -> bool {
        self.types.insert(t.to_smolstr(), t).is_none()
    }
    /// Insert a type into the type register with a specified name.
    ///
    /// Returns false if it replaced an existing type.
    pub fn insert_type_with_name(&mut self, t: Type, name: SmolStr) -> bool {
        self.types.insert(name, t).is_none()
    }

    fn builtin_internal() -> Self {
        let mut register = TypeRegister::default();

        register.insert_type(Type::Float32);
        register.insert_type(Type::Int32);
        register.insert_type(Type::String);
        register.insert_type(Type::PhysicalLength);
        register.insert_type(Type::LogicalLength);
        register.insert_type(Type::Color);
        register.insert_type(Type::ComponentFactory);
        register.insert_type(Type::Duration);
        register.insert_type(Type::Image);
        register.insert_type(Type::Bool);
        register.insert_type(Type::Model);
        register.insert_type(Type::Percent);
        register.insert_type(Type::Easing);
        register.insert_type(Type::Angle);
        register.insert_type(Type::Brush);
        register.insert_type(Type::Rem);
        register.insert_type(Type::StyledText);
        register.insert_type(Type::Keys);
        register.insert_type(Type::DataTransfer);
        register.types.insert("Point".into(), logical_point_type().into());
        register.types.insert("Size".into(), logical_size_type().into());

        BUILTIN.with(|e| e.enums.fill_register(&mut register));

        register.supported_property_animation_types.insert(Type::Float32.to_string());
        register.supported_property_animation_types.insert(Type::Int32.to_string());
        register.supported_property_animation_types.insert(Type::Color.to_string());
        register.supported_property_animation_types.insert(Type::PhysicalLength.to_string());
        register.supported_property_animation_types.insert(Type::LogicalLength.to_string());
        register.supported_property_animation_types.insert(Type::Brush.to_string());
        register.supported_property_animation_types.insert(Type::Angle.to_string());

        macro_rules! register_builtin_structs {
            ($(
                $(#[$attr:meta])*
                $vis:vis struct $Name:ident {
                    $( $(#[$field_attr:meta])* $field:ident : $field_type:ident, )*
                }
            )*) => { $(
                register.insert_type_with_name(Type::Struct(builtin_structs::$Name()), SmolStr::new(stringify!($Name)));
            )* };
        }
        i_slint_common::for_each_builtin_structs!(register_builtin_structs);

        crate::load_builtins::load_builtins(&mut register);

        // Walk every builtin reachable from an exported one and register each
        // accepted child as context-restricted to its parent, so internal types
        // like `MenuItem` report "can only be within Menu" instead of "Unknown".
        let mut visited: HashSet<SmolStr> = HashSet::new();
        let mut to_visit: Vec<Rc<BuiltinElement>> = register
            .elements
            .values()
            .filter_map(|e| match e {
                ElementType::Builtin(b) => Some(b.clone()),
                _ => None,
            })
            .collect();
        while let Some(b) = to_visit.pop() {
            let parent = b.native_class.class_name.clone();
            if !visited.insert(parent.clone()) {
                continue;
            }
            for (child_name, child_type) in &b.additional_accepted_child_types {
                register
                    .context_restricted_types
                    .entry(child_name.clone())
                    .or_default()
                    .insert(parent.clone());
                to_visit.push(child_type.clone());
            }
            if b.additional_accept_self {
                register.context_restricted_types.entry(parent.clone()).or_default().insert(parent);
            }
        }

        match &mut register.elements.get_mut("PopupWindow").unwrap() {
            ElementType::Builtin(b) => {
                let popup = Rc::get_mut(b).unwrap();
                popup.properties.insert(
                    "show".into(),
                    BuiltinPropertyInfo::from(BuiltinFunction::ShowPopupWindow),
                );

                popup.properties.insert(
                    "close".into(),
                    BuiltinPropertyInfo::from(BuiltinFunction::ClosePopupWindow),
                );

                popup.properties.get_mut("close-on-click").unwrap().property_visibility =
                    PropertyVisibility::Constexpr;

                popup.properties.get_mut("close-policy").unwrap().property_visibility =
                    PropertyVisibility::Constexpr;
            }
            _ => unreachable!(),
        };

        match &mut register.elements.get_mut("Timer").unwrap() {
            ElementType::Builtin(b) => {
                let timer = Rc::get_mut(b).unwrap();
                // `start` / `stop` / `restart` are declared as stub
                // functions in `builtins.slint` so their doc comments get
                // picked up, then replaced here with the real builtin
                // implementations. Carry the docs over onto the
                // replacements.
                for (name, func) in [
                    ("start", BuiltinFunction::StartTimer),
                    ("stop", BuiltinFunction::StopTimer),
                    ("restart", BuiltinFunction::RestartTimer),
                ] {
                    let existing_docs = timer.properties.get(name).and_then(|p| p.docs.clone());
                    let mut info = BuiltinPropertyInfo::from(func);
                    info.docs = existing_docs;
                    timer.properties.insert(name.into(), info);
                }
            }
            _ => unreachable!(),
        }

        let font_metrics_prop = crate::langtype::BuiltinPropertyInfo {
            ty: font_metrics_type(),
            property_visibility: PropertyVisibility::Output,
            default_value: BuiltinPropertyDefault::WithElement(|elem| {
                crate::expression_tree::Expression::FunctionCall {
                    function: BuiltinFunction::ItemFontMetrics.into(),
                    arguments: vec![crate::expression_tree::Expression::ElementReference(
                        Rc::downgrade(elem),
                    )],
                    source_location: None,
                }
            }),
            docs: None,
        };

        match &mut register.elements.get_mut("TextInput").unwrap() {
            ElementType::Builtin(b) => {
                let text_input = Rc::get_mut(b).unwrap();
                // Replace the stub function with the real builtin
                // implementation, carrying over docs and arg names.
                let existing = text_input.properties.get("set-selection-offsets");
                let existing_docs = existing.and_then(|p| p.docs.clone());
                let arg_names = existing.and_then(|p| {
                    if let Type::Function(f) = &p.ty { Some(f.arg_names.clone()) } else { None }
                });
                let mut info = BuiltinPropertyInfo::from(BuiltinFunction::SetSelectionOffsets);
                info.docs = existing_docs;
                if let (Some(names), Type::Function(f)) = (arg_names, &info.ty) {
                    let mut func = (**f).clone();
                    // The BuiltinFunction type includes an implicit ElementReference
                    // first arg; skip it to match the public-facing arg names.
                    func.arg_names =
                        std::iter::repeat_n(SmolStr::default(), func.args.len() - names.len())
                            .chain(names)
                            .collect();
                    info.ty = Type::Function(Rc::new(func));
                }
                text_input.properties.insert("set-selection-offsets".into(), info);
                text_input.properties.insert("font-metrics".into(), font_metrics_prop.clone());
            }

            _ => unreachable!(),
        };

        match &mut register.elements.get_mut("Text").unwrap() {
            ElementType::Builtin(b) => {
                let text = Rc::get_mut(b).unwrap();
                text.properties.insert("font-metrics".into(), font_metrics_prop);
            }

            _ => unreachable!(),
        };

        match &mut register.elements.get_mut("Path").unwrap() {
            ElementType::Builtin(b) => {
                let path = Rc::get_mut(b).unwrap();
                path.properties.get_mut("commands").unwrap().property_visibility =
                    PropertyVisibility::Fake;
            }

            _ => unreachable!(),
        };

        match &mut register.elements.get_mut("TabWidget").unwrap() {
            ElementType::Builtin(b) => {
                let tabwidget = Rc::get_mut(b).unwrap();
                tabwidget.properties.get_mut("orientation").unwrap().property_visibility =
                    PropertyVisibility::Constexpr;
            }
            _ => unreachable!(),
        }

        register
    }

    #[doc(hidden)]
    /// All builtins incl. experimental ones! Do not use in production code!
    pub fn builtin_experimental() -> Rc<RefCell<Self>> {
        let register = Self::builtin_internal();
        Rc::new(RefCell::new(register))
    }

    pub fn builtin() -> Rc<RefCell<Self>> {
        let mut register = Self::builtin_internal();

        register.elements.remove("ComponentContainer").unwrap();
        register.types.remove("component-factory").unwrap();

        register.elements.remove("FlexboxLayout").unwrap();
        register.types.remove("FlexboxLayoutDirection").unwrap();
        register.types.remove("FlexboxLayoutAlignContent").unwrap();
        register.types.remove("FlexboxLayoutWrap").unwrap();
        register.types.remove("FlexboxLayoutAlignSelf").unwrap();

        Rc::new(RefCell::new(register))
    }

    pub fn new(parent: &Rc<RefCell<TypeRegister>>) -> Self {
        Self {
            parent_registry: Some(parent.clone()),
            expose_internal_types: parent.borrow().expose_internal_types,
            ..Default::default()
        }
    }

    pub fn lookup(&self, name: &str) -> Type {
        self.types
            .get(name)
            .cloned()
            .or_else(|| self.parent_registry.as_ref().map(|r| r.borrow().lookup(name)))
            .unwrap_or_default()
    }

    fn lookup_element_as_result(
        &self,
        name: &str,
    ) -> Result<ElementType, HashMap<SmolStr, HashSet<SmolStr>>> {
        match self.elements.get(name).cloned() {
            Some(ty) => Ok(ty),
            None => match &self.parent_registry {
                Some(r) => r.borrow().lookup_element_as_result(name),
                None => Err(self.context_restricted_types.clone()),
            },
        }
    }

    pub fn lookup_element(&self, name: &str) -> Result<ElementType, String> {
        self.lookup_element_as_result(name).map_err(|context_restricted_types| {
            if let Some(permitted_parent_types) = context_restricted_types.get(name) {
                if permitted_parent_types.len() == 1 {
                    format!(
                        "{} can only be within a {} element",
                        name,
                        permitted_parent_types.iter().next().unwrap()
                    )
                } else {
                    let mut elements = permitted_parent_types.iter().cloned().collect::<Vec<_>>();
                    elements.sort();
                    format!(
                        "{} can only be within the following elements: {}",
                        name,
                        elements.join(", ")
                    )
                }
            } else if let Some(ty) = self.types.get(name) {
                format!("'{ty}' cannot be used as an element")
            } else {
                format!("Unknown element '{name}'")
            }
        })
    }

    pub fn lookup_builtin_element(&self, name: &str) -> Option<ElementType> {
        self.parent_registry.as_ref().map_or_else(
            || self.elements.get(name).cloned(),
            |p| p.borrow().lookup_builtin_element(name),
        )
    }

    pub fn lookup_qualified<Member: AsRef<str>>(&self, qualified: &[Member]) -> Type {
        if qualified.len() != 1 {
            return Type::Invalid;
        }
        self.lookup(qualified[0].as_ref())
    }

    /// Add the component with its defined name
    ///
    /// Returns false if there was already an element with the same name
    pub fn add(&mut self, comp: Rc<Component>) -> bool {
        self.add_with_name(comp.id.clone(), comp)
    }

    /// Add the component with a specified name
    ///
    /// Returns false if there was already an element with the same name
    pub fn add_with_name(&mut self, name: SmolStr, comp: Rc<Component>) -> bool {
        self.elements.insert(name, ElementType::Component(comp)).is_none()
    }

    pub fn add_builtin(&mut self, builtin: Rc<BuiltinElement>) {
        self.elements.insert(builtin.name.clone(), ElementType::Builtin(builtin));
    }

    pub fn property_animation_type_for_property(&self, property_type: Type) -> ElementType {
        if self.supported_property_animation_types.contains(&property_type.to_string()) {
            self.property_animation_type.clone()
        } else {
            self.parent_registry
                .as_ref()
                .map(|registry| {
                    registry.borrow().property_animation_type_for_property(property_type)
                })
                .unwrap_or_default()
        }
    }

    /// Return a hashmap with all the registered type
    pub fn all_types(&self) -> HashMap<SmolStr, Type> {
        let mut all =
            self.parent_registry.as_ref().map(|r| r.borrow().all_types()).unwrap_or_default();
        for (k, v) in &self.types {
            all.insert(k.clone(), v.clone());
        }
        all
    }

    /// Return a hashmap with all the registered element type
    pub fn all_elements(&self) -> HashMap<SmolStr, ElementType> {
        let mut all =
            self.parent_registry.as_ref().map(|r| r.borrow().all_elements()).unwrap_or_default();
        for (k, v) in &self.elements {
            all.insert(k.clone(), v.clone());
        }
        all
    }

    pub fn empty_type(&self) -> ElementType {
        match self.parent_registry.as_ref() {
            Some(parent) => parent.borrow().empty_type(),
            None => self.empty_type.clone(),
        }
    }
}

#[allow(dead_code)]
pub(crate) fn element_type_kind(element_type: &ElementType) -> &'static str {
    match element_type {
        ElementType::Builtin(_) => "builtin",
        ElementType::Component(_) => "component",
        ElementType::Native(_) => "native",
        ElementType::Error => "error",
        ElementType::Global => "global",
        ElementType::Interface => "interface",
    }
}

#[allow(dead_code)]
fn primitive_type_from_name(name: &str) -> Option<Type> {
    Some(match name {
        "float" => Type::Float32,
        "int" => Type::Int32,
        "string" => Type::String,
        "physical-length" => Type::PhysicalLength,
        "length" => Type::LogicalLength,
        "color" => Type::Color,
        "component-factory" => Type::ComponentFactory,
        "duration" => Type::Duration,
        "image" => Type::Image,
        "bool" => Type::Bool,
        "model" => Type::Model,
        "percent" => Type::Percent,
        "easing" => Type::Easing,
        "angle" => Type::Angle,
        "brush" => Type::Brush,
        "relative-font-size" => Type::Rem,
        "styled-text" => Type::StyledText,
        "keys" => Type::Keys,
        "data-transfer" => Type::DataTransfer,
        _ => return None,
    })
}

#[allow(dead_code)]
fn builtin_type_from_name(name: &str) -> Option<Type> {
    primitive_type_from_name(name).or_else(|| match name {
        "Point" => Some(Type::Struct(logical_point_type())),
        "Size" => Some(Type::Struct(logical_size_type())),
        _ => None,
    })
}

fn rehydrate_accepted_child_builtin(
    name: &str,
    frozen_elements_by_name: &HashMap<&str, &crate::frozen_builtins::FrozenBuiltinRegistryElement>,
    registry: &TypeRegister,
) -> Option<Rc<BuiltinElement>> {
    let mut child = match registry.elements.get(name) {
        Some(ElementType::Builtin(child)) => (**child).clone(),
        _ => registry
            .elements
            .values()
            .find_map(|element_type| {
                let ElementType::Builtin(child) = element_type else {
                    return None;
                };
                (child.native_class.class_name == name).then(|| (**child).clone())
            })
            .or_else(|| {
                let frozen_child = frozen_elements_by_name.get(name)?;
                (frozen_child.kind == "builtin")
                    .then(|| rehydrate_builtin_registry_element(frozen_child, registry))
            })?,
    };

    if let Some(frozen_child) = frozen_elements_by_name.get(name) {
        child.additional_accepted_child_types = frozen_child
            .accepted_child_types
            .iter()
            .filter_map(|child_name| {
                Some((
                    SmolStr::new(child_name.as_str()),
                    rehydrate_accepted_child_builtin(
                        child_name,
                        frozen_elements_by_name,
                        registry,
                    )?,
                ))
            })
            .collect();
    }

    Some(Rc::new(child))
}

#[allow(dead_code)]
fn default_size_binding_from_name(name: &str) -> DefaultSizeBinding {
    match name {
        "ExpandsToParentGeometry" => DefaultSizeBinding::ExpandsToParentGeometry,
        "ImplicitSize" => DefaultSizeBinding::ImplicitSize,
        _ => DefaultSizeBinding::None,
    }
}

fn freeze_builtin_registry_element(
    name: &str,
    kind: &str,
    builtin: &BuiltinElement,
) -> crate::frozen_builtins::FrozenBuiltinRegistryElement {
    let mut accepted_child_types =
        builtin.additional_accepted_child_types.keys().map(ToString::to_string).collect::<Vec<_>>();
    accepted_child_types.sort();

    crate::frozen_builtins::FrozenBuiltinRegistryElement {
        name: name.to_string(),
        kind: kind.into(),
        native_class: builtin.native_class.class_name.to_string(),
        native_class_hierarchy: freeze_native_class_hierarchy(&builtin.native_class),
        property_count: builtin.properties.len(),
        properties: freeze_builtin_registry_properties(&builtin.properties),
        native_properties: freeze_native_registry_properties(&builtin.native_class.properties),
        accepted_child_types,
        additional_accept_self: builtin.additional_accept_self,
        accepts_focus: builtin.accepts_focus,
        is_global: builtin.is_global,
        is_internal: builtin.is_internal,
        is_non_item_type: builtin.is_non_item_type,
        default_size_binding: format!("{:?}", builtin.default_size_binding),
        ..Default::default()
    }
}

fn freeze_component_registry_element(
    name: &str,
    kind: &str,
    component: &Component,
) -> crate::frozen_builtins::FrozenBuiltinRegistryElement {
    let root_element = component.root_element.borrow();
    let mut frozen = if let ElementType::Builtin(builtin) = &root_element.base_type {
        freeze_builtin_registry_element(name, kind, builtin)
    } else {
        crate::frozen_builtins::FrozenBuiltinRegistryElement {
            name: name.to_string(),
            kind: kind.into(),
            ..Default::default()
        }
    };
    frozen.component_root_id = root_element.id.to_string();
    frozen.component_root_base_kind = element_type_kind(&root_element.base_type).into();
    frozen.component_root_base_type =
        root_element.base_type.type_name().map(str::to_string).unwrap_or_default();
    frozen.component_root_properties = root_element
        .property_declarations
        .iter()
        .map(|(name, declaration)| crate::frozen_builtins::FrozenBuiltinPropertyDeclaration {
            name: name.to_string(),
            ty: declaration.property_type.to_string(),
            visibility: declaration.visibility.to_string(),
        })
        .collect();
    frozen
}

fn freeze_builtin_registry_properties(
    properties: &BTreeMap<SmolStr, BuiltinPropertyInfo>,
) -> Vec<crate::frozen_builtins::FrozenBuiltinRegistryProperty> {
    properties
        .iter()
        .map(|(name, property)| freeze_builtin_registry_property(name, property))
        .collect()
}

fn freeze_native_registry_properties(
    properties: &HashMap<SmolStr, BuiltinPropertyInfo>,
) -> Vec<crate::frozen_builtins::FrozenBuiltinRegistryProperty> {
    let mut properties = properties
        .iter()
        .map(|(name, property)| freeze_builtin_registry_property(name, property))
        .collect::<Vec<_>>();
    properties.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));
    properties
}

fn freeze_native_class_hierarchy(
    native_class: &Rc<NativeClass>,
) -> Vec<crate::frozen_builtins::FrozenBuiltinNativeClass> {
    let mut hierarchy =
        native_class.parent.as_ref().map(freeze_native_class_hierarchy).unwrap_or_default();
    hierarchy.push(crate::frozen_builtins::FrozenBuiltinNativeClass {
        name: native_class.class_name.to_string(),
        properties: freeze_native_registry_properties(&native_class.properties),
    });
    hierarchy
}

fn freeze_builtin_registry_property(
    name: &SmolStr,
    property: &BuiltinPropertyInfo,
) -> crate::frozen_builtins::FrozenBuiltinRegistryProperty {
    let (default_kind, builtin_function) = match &property.default_value {
        BuiltinPropertyDefault::None => ("none".into(), None),
        BuiltinPropertyDefault::Expr(_) => ("expr".into(), None),
        BuiltinPropertyDefault::WithElement(_) => ("with-element".into(), None),
        BuiltinPropertyDefault::BuiltinFunction(function) => {
            ("builtin-function".into(), Some(format!("{function:?}")))
        }
    };

    crate::frozen_builtins::FrozenBuiltinRegistryProperty {
        name: name.to_string(),
        ty: property.ty.to_string(),
        visibility: property.property_visibility.to_string(),
        default_kind,
        builtin_function,
    }
}

fn rehydrate_builtin_registry_element(
    frozen_element: &crate::frozen_builtins::FrozenBuiltinRegistryElement,
    registry: &TypeRegister,
) -> BuiltinElement {
    let native_class = rehydrate_native_class(frozen_element, registry);
    let mut builtin = BuiltinElement::new(Rc::new(native_class));
    builtin.name = frozen_element.name.as_str().into();
    builtin.properties = frozen_element
        .properties
        .iter()
        .map(|property| {
            (
                SmolStr::new(property.name.as_str()),
                BuiltinPropertyInfo {
                    ty: rehydrate_registry_property_type(&property.ty, registry),
                    property_visibility: visibility_from_name(&property.visibility),
                    default_value: rehydrate_builtin_property_default(property),
                    docs: None,
                },
            )
        })
        .collect();
    builtin.additional_accept_self = frozen_element.additional_accept_self;
    builtin.accepts_focus = frozen_element.accepts_focus;
    builtin.is_global = frozen_element.is_global;
    builtin.is_internal = frozen_element.is_internal;
    builtin.is_non_item_type = frozen_element.is_non_item_type;
    builtin.default_size_binding =
        default_size_binding_from_name(frozen_element.default_size_binding.as_str());
    builtin
}

fn rehydrate_native_class(
    frozen_element: &crate::frozen_builtins::FrozenBuiltinRegistryElement,
    registry: &TypeRegister,
) -> NativeClass {
    let mut parent = None;
    for frozen_class in &frozen_element.native_class_hierarchy {
        let mut native_class = NativeClass::new_with_properties(
            frozen_class.name.as_str(),
            frozen_class.properties.iter().map(|property| {
                (
                    SmolStr::new(property.name.as_str()),
                    BuiltinPropertyInfo {
                        ty: rehydrate_registry_property_type(&property.ty, registry),
                        property_visibility: visibility_from_name(&property.visibility),
                        default_value: rehydrate_builtin_property_default(property),
                        docs: None,
                    },
                )
            }),
        );
        native_class.parent = parent;
        parent = Some(Rc::new(native_class));
    }

    parent
        .map(|native_class| (*native_class).clone())
        .unwrap_or_else(|| NativeClass::new(frozen_element.native_class.as_str()))
}

fn rehydrate_registry_component_root_base_type(
    registry: &TypeRegister,
    frozen_element: &crate::frozen_builtins::FrozenBuiltinRegistryElement,
) -> ElementType {
    match frozen_element.component_root_base_kind.as_str() {
        "builtin" => ElementType::Builtin(Rc::new(rehydrate_builtin_registry_element(
            frozen_element,
            registry,
        ))),
        kind => rehydrate_registry_element_base_type(
            registry,
            kind,
            frozen_element.component_root_base_type.as_str(),
        ),
    }
}

fn rehydrate_registry_property_type(name: &str, registry: &TypeRegister) -> Type {
    let name = normalized_frozen_type_name(name);
    let ty = registry.lookup(name);
    if ty != Type::Invalid {
        return ty;
    }
    builtin_type_from_name(name).unwrap_or_else(|| match name {
        "element ref" => Type::ElementReference,
        "void" => Type::Void,
        _ => Type::Invalid,
    })
}

fn rehydrate_registry_element_base_type(
    registry: &TypeRegister,
    kind: &str,
    name: &str,
) -> ElementType {
    match kind {
        "global" => ElementType::Global,
        "interface" => ElementType::Interface,
        "builtin" | "component" => registry.lookup_element(name).unwrap_or(ElementType::Error),
        _ => ElementType::Error,
    }
}

fn rehydrate_registry_element_reference(registry: &TypeRegister, name: &str) -> ElementType {
    registry
        .elements
        .values()
        .find_map(|element_type| {
            let builtin = match element_type {
                ElementType::Builtin(builtin) => Some(builtin.clone()),
                ElementType::Component(component) => {
                    let ElementType::Builtin(builtin) = &component.root_element.borrow().base_type
                    else {
                        return None;
                    };
                    Some(builtin.clone())
                }
                _ => None,
            }?;
            (builtin.name == name || builtin.native_class.class_name == name)
                .then(|| ElementType::Builtin(builtin))
        })
        .or_else(|| registry.lookup_element(name).ok())
        .unwrap_or_else(|| {
            let mut builtin = BuiltinElement::new(Rc::new(NativeClass::new(name)));
            builtin.name = SmolStr::new(name);
            ElementType::Builtin(Rc::new(builtin))
        })
}

fn rehydrate_root_property_declarations(
    properties: &[crate::frozen_builtins::FrozenBuiltinPropertyDeclaration],
    registry: &TypeRegister,
) -> BTreeMap<SmolStr, PropertyDeclaration> {
    properties
        .iter()
        .map(|property| {
            (
                SmolStr::new(property.name.as_str()),
                PropertyDeclaration {
                    property_type: rehydrate_registry_declaration_type(&property.ty, registry),
                    visibility: visibility_from_name(&property.visibility),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn rehydrate_registry_declaration_type(name: &str, registry: &TypeRegister) -> Type {
    let name = normalized_frozen_type_name(name);
    let ty = registry.lookup(name);
    if ty != Type::Invalid {
        return ty;
    }
    rehydrate_registry_property_type(name, registry)
}

fn normalized_frozen_type_name(name: &str) -> &str {
    name.strip_prefix("enum ").or_else(|| name.strip_prefix("struct ")).unwrap_or(name)
}

fn visibility_from_name(name: &str) -> PropertyVisibility {
    match name {
        "input" => PropertyVisibility::Input,
        "output" => PropertyVisibility::Output,
        "input output" => PropertyVisibility::InOut,
        "constexpr" => PropertyVisibility::Constexpr,
        "public" => PropertyVisibility::Public,
        "protected" => PropertyVisibility::Protected,
        "fake" => PropertyVisibility::Fake,
        _ => PropertyVisibility::Private,
    }
}

fn rehydrate_builtin_property_default(
    property: &crate::frozen_builtins::FrozenBuiltinRegistryProperty,
) -> BuiltinPropertyDefault {
    if property.default_kind != "builtin-function" {
        return BuiltinPropertyDefault::None;
    }
    property
        .builtin_function
        .as_deref()
        .and_then(builtin_function_from_name)
        .map(BuiltinPropertyDefault::BuiltinFunction)
        .unwrap_or(BuiltinPropertyDefault::None)
}

fn builtin_function_from_name(name: &str) -> Option<BuiltinFunction> {
    Some(match name {
        "SetFocusItem" => BuiltinFunction::SetFocusItem,
        "ClearFocusItem" => BuiltinFunction::ClearFocusItem,
        "ShowPopupWindow" => BuiltinFunction::ShowPopupWindow,
        "ClosePopupWindow" => BuiltinFunction::ClosePopupWindow,
        "SetSelectionOffsets" => BuiltinFunction::SetSelectionOffsets,
        "StartTimer" => BuiltinFunction::StartTimer,
        "StopTimer" => BuiltinFunction::StopTimer,
        "RestartTimer" => BuiltinFunction::RestartTimer,
        _ => return None,
    })
}

/// Type definitions for each builtin struct
pub mod builtin_structs {
    use super::*;

    thread_local! {
        pub static BUILTIN_STRUCTS: BuiltinStructs = BuiltinStructs::new();
    }

    #[rustfmt::skip]
    macro_rules! map_type {
        ($pub_type:ident, bool) => { Type::Bool };
        ($pub_type:ident, i32) => { Type::Int32 };
        ($pub_type:ident, f32) => { Type::Float32 };
        ($pub_type:ident, SharedString) => { Type::String };
        ($pub_type:ident, Image) => { Type::Image };
        ($pub_type:ident, Coord) => { Type::LogicalLength };
        ($pub_type:ident, Keys) => { Type::Keys };
        ($pub_type:ident, DataTransfer) => { Type::DataTransfer };
        ($pub_type:ident, LogicalPosition) => { Type::Struct(logical_point_type()) };
        ($pub_type:ident, LogicalSize) => { Type::Struct(logical_size_type()) };
        // builtin structs
        ($pub_type:ident, KeyboardModifiers) => {
            // Note, this references the local variable in the BuiltinStructs constructor
            Type::Struct($pub_type.clone())
        };
        // builtin enums
        ($pub_type:ident, $_:ident) => {
            BUILTIN.with(|e| Type::Enumeration(e.enums.$pub_type.clone()))
        };
    }

    macro_rules! declare_builtin_structs {
        ($(
            $(#[$attr:meta])*
            $vis:vis struct $Name:ident {
                $( $(#[$field_attr:meta])* $field:ident : $field_type:ident, )*
            }
        )*) => {
            pub struct BuiltinStructs {
                $(
                #[allow(non_snake_case)]
                $Name: Rc<Struct>
                ),*
            }
            impl BuiltinStructs {
                pub fn new() -> Self {
                    $(
                    #[allow(non_snake_case)]
                    let $Name = Rc::new(Struct{
                        fields: BTreeMap::from([
                            $((stringify!($field).replace_smolstr("_", "-"), map_type!($field_type, $field_type))),*
                        ]),
                        name: BuiltinStruct::$Name.into(),
                    });
                    )*

                    Self {
                        $($Name),*
                    }
                }
            }

            impl Default for BuiltinStructs {
                fn default() -> Self {
                    Self::new()
                }
            }

            $(
            #[allow(non_snake_case)]
            pub fn $Name() -> Rc<Struct> {
                BUILTIN_STRUCTS.with(|types| types.$Name.clone())
            }
            )*
        };
    }
    i_slint_common::for_each_builtin_structs!(declare_builtin_structs);
}

pub fn logical_point_type() -> Rc<Struct> {
    BUILTIN.with(|types| types.logical_point_type.clone())
}

pub fn logical_size_type() -> Rc<Struct> {
    BUILTIN.with(|types| types.logical_size_type.clone())
}

pub fn font_metrics_type() -> Type {
    BUILTIN.with(|types| types.font_metrics_type.clone())
}

/// The [`Type`] for a runtime LayoutInfo structure
pub fn layout_info_type() -> Rc<Struct> {
    BUILTIN.with(|types| types.layout_info_type.clone())
}

/// The [`Type`] for a runtime PathElement structure
pub fn path_element_type() -> Type {
    BUILTIN.with(|types| types.path_element_type.clone())
}

/// The [`Type`] for a runtime LayoutItemInfo structure
pub fn layout_item_info_type() -> Type {
    BUILTIN.with(|types| types.layout_item_info_type.clone())
}

/// The [`Type`] for a runtime FlexboxLayoutItemInfo structure
pub fn flexbox_layout_item_info_type() -> Type {
    BUILTIN.with(|types| types.flexbox_layout_item_info_type.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_builtin_registry_metadata_is_process_global_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::frozen_builtins::FrozenBuiltinRegistry>();

        let registry = TypeRegister::builtin();
        let frozen = registry.borrow().freeze_builtin_registry_metadata();

        assert!(frozen.types.iter().any(|ty| ty == "length"));
        assert!(frozen.supported_property_animation_types.iter().any(|ty| ty == "length"));
        assert!(frozen.elements.iter().any(|element| {
            element.name == "Rectangle"
                && element.kind == "builtin"
                && !element.native_class.is_empty()
                && element.property_count > 0
        }));
        assert!(frozen
            .context_restricted_types
            .iter()
            .any(|restriction| !restriction.name.is_empty() && !restriction.contexts.is_empty()));
        assert!(frozen.elements.iter().any(|element| {
            element.name == "Platform"
                && element.kind == "component"
                && element.component_root_base_kind == "builtin"
                && element.is_global
                && element.properties.iter().any(|property| property.name == "os")
        }));

        let rehydrated = TypeRegister::rehydrate_builtin_registry_shell(&frozen);
        assert_eq!(rehydrated.borrow().lookup("length"), Type::LogicalLength);
        assert!(matches!(
            rehydrated.borrow().lookup_element("Rectangle"),
            Ok(ElementType::Builtin(_))
        ));
        let rectangle = rehydrated.borrow().lookup_element("Rectangle").unwrap();
        let background = rectangle.lookup_property("background");
        assert_eq!(background.property_type, Type::Brush);
        assert_eq!(background.property_visibility, PropertyVisibility::Input);

        let text = rehydrated.borrow().lookup_element("Text").unwrap();
        let text_prop = text.lookup_property("text");
        assert_eq!(text_prop.property_type, Type::String);
        assert_eq!(text_prop.property_visibility, PropertyVisibility::Input);
        assert!(matches!(rehydrated.borrow().lookup("MenuEntry"), Type::Struct(_)));

        let popup_window = rehydrated.borrow().lookup_element("PopupWindow").unwrap();
        assert_eq!(
            popup_window.lookup_property("show").builtin_function,
            Some(BuiltinFunction::ShowPopupWindow)
        );
        assert_eq!(
            popup_window.lookup_property("close").builtin_function,
            Some(BuiltinFunction::ClosePopupWindow)
        );

        let timer = rehydrated.borrow().lookup_element("Timer").unwrap();
        assert_eq!(
            timer.lookup_property("start").builtin_function,
            Some(BuiltinFunction::StartTimer)
        );
        assert_eq!(
            timer.lookup_property("stop").builtin_function,
            Some(BuiltinFunction::StopTimer)
        );
        assert_eq!(
            timer.lookup_property("restart").builtin_function,
            Some(BuiltinFunction::RestartTimer)
        );

        let text_input = rehydrated.borrow().lookup_element("TextInput").unwrap();
        assert_eq!(
            text_input.lookup_property("set-selection-offsets").builtin_function,
            Some(BuiltinFunction::SetSelectionOffsets)
        );

        let platform = rehydrated.borrow().lookup_element("Platform").unwrap();
        let ElementType::Component(platform) = platform else {
            panic!("expected Platform to rehydrate as a component");
        };
        assert!(platform.is_global());
        let os = platform.root_element.borrow().lookup_property("os");
        assert_eq!(os.property_type.to_string(), "enum OperatingSystemType");
        assert_eq!(os.property_visibility, PropertyVisibility::Output);

        let native_palette = rehydrated.borrow().lookup_element("NativePalette").unwrap();
        let ElementType::Component(native_palette) = native_palette else {
            panic!("expected NativePalette to rehydrate as a component");
        };
        assert!(native_palette.is_global());
        let background = native_palette.root_element.borrow().lookup_property("background");
        assert_eq!(background.property_type, Type::Brush);
        assert_eq!(background.property_visibility, PropertyVisibility::Output);

        let rehydrated_registry = rehydrated.borrow();
        let (frozen_parent, rehydrated_parent) = frozen
            .elements
            .iter()
            .filter(|element| !element.accepted_child_types.is_empty())
            .find_map(|frozen_parent| {
                let rehydrated_parent =
                    rehydrated_registry.lookup_element(&frozen_parent.name).ok()?;
                (!rehydrated_parent.as_builtin().additional_accepted_child_types.is_empty())
                    .then_some((frozen_parent, rehydrated_parent))
            })
            .expect("expected at least one rehydrated builtin with accepted child types");
        assert!(
            rehydrated_parent
                .as_builtin()
                .additional_accepted_child_types
                .keys()
                .any(|child| frozen_parent.accepted_child_types.contains(&child.to_string()))
        );

        let context_menu_area =
            rehydrated_registry.lookup_builtin_element("ContextMenuArea").unwrap();
        let menu = context_menu_area
            .as_builtin()
            .additional_accepted_child_types
            .get("Menu")
            .expect("ContextMenuArea should accept Menu");
        assert!(
            menu.additional_accepted_child_types.contains_key("MenuItem"),
            "Menu should accept MenuItem after frozen registry rehydration"
        );
    }
}
