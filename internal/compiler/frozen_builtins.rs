// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

/*!
    Process-global-safe metadata for precompiled builtin libraries.

    This is an experimental stepping stone toward a rehydratable builtin artifact. The
    current object tree is built around `Rc`, `RefCell`, `Weak`, and syntax-node handles,
    so it cannot be stored directly in a process-global cache. These structs intentionally
    contain only owned, pointer-free data.
*/

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::{Mutex, OnceLock};

use smol_str::SmolStr;

use crate::CompilerConfiguration;
use crate::expression_tree::{Callable, Expression, TwoWayBinding};
use crate::langtype::{ElementType, Enumeration, Function, Struct, StructName, Type};
use crate::namedreference::NamedReference;
use crate::object_tree::{Component, Element, ElementRc, PropertyDeclaration, PropertyVisibility};
use crate::typeregister::TypeRegister;

pub(crate) const FROZEN_BUILTIN_SCHEMA_VERSION: u32 = 10;

mod generated_builtin_artifacts {
    include!(concat!(env!("OUT_DIR"), "/frozen_builtin_artifacts.rs"));
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinCacheKey {
    pub(crate) resolved_style: String,
    pub(crate) enable_experimental: bool,
    pub(crate) debug_hooks: bool,
    pub(crate) translation_domain: Option<String>,
    pub(crate) default_translation_context: FrozenDefaultTranslationContext,
}

impl FrozenBuiltinCacheKey {
    pub(crate) fn from_config(
        compiler_config: &CompilerConfiguration,
        resolved_style: &str,
    ) -> Option<Self> {
        // Prototype constraint: include paths are allowed to override `std-widgets.slint` and
        // style files, so only cache the pure embedded-builtin configuration for now.
        if !compiler_config.include_paths.is_empty() {
            return None;
        }
        if compiler_config.open_import_callback.is_some()
            || compiler_config.resource_url_mapper.is_some()
        {
            return None;
        }

        let known_builtin_style =
            crate::fileaccess::styles().into_iter().any(|style| style == resolved_style);
        if !known_builtin_style {
            return None;
        }

        Some(Self {
            resolved_style: resolved_style.into(),
            enable_experimental: compiler_config.enable_experimental,
            debug_hooks: compiler_config.debug_hooks.is_some(),
            translation_domain: compiler_config.translation_domain.clone(),
            default_translation_context: (&compiler_config.default_translation_context).into(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) enum FrozenDefaultTranslationContext {
    ComponentName,
    None,
}

impl From<&crate::DefaultTranslationContext> for FrozenDefaultTranslationContext {
    fn from(value: &crate::DefaultTranslationContext) -> Self {
        match value {
            crate::DefaultTranslationContext::ComponentName => Self::ComponentName,
            crate::DefaultTranslationContext::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinLibrary {
    pub(crate) schema_version: u32,
    pub(crate) parent_registry: FrozenBuiltinRegistry,
    pub(crate) documents: Vec<FrozenBuiltinDocument>,
}

impl FrozenBuiltinLibrary {
    pub(crate) fn is_supported_schema_version(&self) -> bool {
        self.schema_version == FROZEN_BUILTIN_SCHEMA_VERSION
    }

    pub(crate) fn rehydrate_parent_registry(&self) -> Rc<RefCell<TypeRegister>> {
        TypeRegister::rehydrate_builtin_registry_shell(&self.parent_registry)
    }

    pub(crate) fn rehydrate_component_skeletons(
        &self,
        parent_registry: &Rc<RefCell<TypeRegister>>,
    ) -> TypeRegister {
        let mut registry = TypeRegister::new(parent_registry);
        for frozen_type in self.documents.iter().flat_map(|doc| &doc.inner_types) {
            if let Some((name, ty)) = Self::rehydrate_document_type(frozen_type, &registry) {
                registry.insert_type_with_name(ty, name);
            }
        }

        let mut components = Vec::new();

        for frozen_document in &self.documents {
            for frozen_component in &frozen_document.components {
                let component = Rc::new(Component {
                    id: frozen_component.id.as_str().into(),
                    root_element: Element::default().make_rc(),
                    ..Default::default()
                });
                registry.add(component.clone());
                registry.add_with_name(
                    SmolStr::new(format!("{}#{}", frozen_document.path, frozen_component.id)),
                    component.clone(),
                );
                components.push((component, frozen_component));
            }
        }

        let context = FrozenBuiltinRehydrationContext::new(&registry);
        for (component, frozen_component) in &components {
            Self::rehydrate_element_skeleton(
                &frozen_component.root_element,
                &component.root_element,
                Rc::downgrade(&component),
                &registry,
                &context,
            );
            if let Some(child_insertion_point) = &frozen_component.child_insertion_point {
                let Some(parent) = Self::element_at_path(
                    &component.root_element,
                    &child_insertion_point.parent_path,
                ) else {
                    continue;
                };
                *component.child_insertion_point.borrow_mut() =
                    Some(crate::object_tree::ChildrenInsertionPoint {
                        parent,
                        insertion_index: child_insertion_point.insertion_index,
                        node: Self::dummy_children_placeholder(),
                    });
            }
        }
        let component_roots = registry
            .all_elements()
            .into_iter()
            .filter_map(|(_, element_type)| {
                let ElementType::Component(component) = element_type else {
                    return None;
                };
                Some((component.id.to_string(), component.root_element.clone()))
            })
            .collect::<HashMap<_, _>>();
        for frozen_component in self.documents.iter().flat_map(|doc| &doc.components) {
            let Some(ElementType::Component(component)) =
                registry.lookup_element(&frozen_component.id).ok()
            else {
                continue;
            };
            Self::rehydrate_element_bindings(
                &frozen_component.root_element,
                &component.root_element,
                &registry,
                &component_roots,
            );
        }

        registry
    }

    pub(crate) fn rehydrate_document_type(
        frozen_type: &FrozenBuiltinType,
        registry: &TypeRegister,
    ) -> Option<(SmolStr, Type)> {
        Some(match frozen_type {
            FrozenBuiltinType::Struct(frozen_struct) => {
                let fields = frozen_struct
                    .fields
                    .iter()
                    .map(|field| {
                        (
                            SmolStr::new(field.name.as_str()),
                            Self::rehydrate_type(&field.ty, registry),
                        )
                    })
                    .collect();
                let name = SmolStr::new(frozen_struct.name.as_str());
                let node = Self::dummy_struct_object_type(&frozen_struct.name);
                (
                    name.clone(),
                    Type::Struct(Rc::new(Struct { fields, name: StructName::User { name, node } })),
                )
            }
            FrozenBuiltinType::Enumeration { name, values, default_value } => {
                let name = SmolStr::new(name.as_str());
                (
                    name.clone(),
                    Type::Enumeration(Rc::new(Enumeration {
                        name,
                        values: values.iter().map(|value| SmolStr::new(value.as_str())).collect(),
                        default_value: *default_value,
                        node: None,
                    })),
                )
            }
        })
    }

    fn rehydrate_element_bindings(
        frozen_element: &FrozenBuiltinElement,
        element: &ElementRc,
        registry: &TypeRegister,
        component_roots: &HashMap<String, ElementRc>,
    ) {
        let bindings = frozen_element
            .bindings
            .iter()
            .filter_map(|binding| {
                Some((
                    SmolStr::new(binding.name.as_str()),
                    RefCell::new(crate::expression_tree::BindingExpression {
                        expression: Self::rehydrate_expression(
                            &binding.expression,
                            registry,
                            component_roots,
                        )?,
                        span: None,
                        priority: binding.priority,
                        animation: None,
                        analysis: binding.analysis_is_const.map(|is_const| {
                            crate::expression_tree::BindingAnalysis {
                                is_in_binding_loop: Default::default(),
                                is_const,
                                no_external_dependencies: false,
                            }
                        }),
                        two_way_bindings: binding
                            .two_way_bindings
                            .iter()
                            .filter_map(|binding| {
                                Self::rehydrate_two_way_binding(binding, component_roots)
                            })
                            .collect(),
                    }),
                ))
            })
            .collect();
        element.borrow_mut().bindings = bindings;
        for (frozen_child, child) in frozen_element.children.iter().zip(&element.borrow().children)
        {
            Self::rehydrate_element_bindings(frozen_child, child, registry, component_roots);
        }
    }

    fn rehydrate_named_reference(
        reference: &FrozenBuiltinNamedReference,
        component_roots: &HashMap<String, ElementRc>,
    ) -> Option<NamedReference> {
        let root = component_roots.get(&reference.component)?;
        Some(NamedReference::new(
            &Self::element_at_path(root, &reference.element_path)?,
            SmolStr::new(reference.property.as_str()),
        ))
    }

    fn rehydrate_element_reference(
        reference: &FrozenBuiltinElementReference,
        component_roots: &HashMap<String, ElementRc>,
    ) -> Option<ElementRc> {
        let root = component_roots.get(&reference.component)?;
        Self::element_at_path(root, &reference.element_path)
    }

    fn rehydrate_two_way_binding(
        binding: &FrozenBuiltinTwoWayBinding,
        component_roots: &HashMap<String, ElementRc>,
    ) -> Option<TwoWayBinding> {
        Some(match binding {
            FrozenBuiltinTwoWayBinding::Property { property, field_access } => {
                TwoWayBinding::Property {
                    property: Self::rehydrate_named_reference(property, component_roots)?,
                    field_access: field_access
                        .iter()
                        .map(|field| SmolStr::new(field.as_str()))
                        .collect(),
                }
            }
            FrozenBuiltinTwoWayBinding::ModelData { repeated_element, field_access } => {
                TwoWayBinding::ModelData {
                    repeated_element: Rc::downgrade(&Self::rehydrate_element_reference(
                        repeated_element,
                        component_roots,
                    )?),
                    field_access: field_access
                        .iter()
                        .map(|field| SmolStr::new(field.as_str()))
                        .collect(),
                }
            }
        })
    }

    fn rehydrate_expression(
        expression: &FrozenBuiltinExpression,
        registry: &TypeRegister,
        component_roots: &HashMap<String, ElementRc>,
    ) -> Option<Expression> {
        Some(match expression {
            FrozenBuiltinExpression::Invalid => Expression::Invalid,
            FrozenBuiltinExpression::StringLiteral(value) => {
                Expression::StringLiteral(SmolStr::new(value.as_str()))
            }
            FrozenBuiltinExpression::NumberLiteral { value, unit } => {
                Expression::NumberLiteral(*value, unit.parse().ok()?)
            }
            FrozenBuiltinExpression::BoolLiteral(value) => Expression::BoolLiteral(*value),
            FrozenBuiltinExpression::PropertyReference(reference) => Expression::PropertyReference(
                Self::rehydrate_named_reference(reference, component_roots)?,
            ),
            FrozenBuiltinExpression::FunctionParameterReference { index, ty } => {
                Expression::FunctionParameterReference {
                    index: *index,
                    ty: Self::rehydrate_type(ty, registry),
                }
            }
            FrozenBuiltinExpression::StoreLocalVariable { name, value } => {
                Expression::StoreLocalVariable {
                    name: SmolStr::new(name.as_str()),
                    value: Box::new(Self::rehydrate_expression(value, registry, component_roots)?),
                }
            }
            FrozenBuiltinExpression::ReadLocalVariable { name, ty } => {
                Expression::ReadLocalVariable {
                    name: SmolStr::new(name.as_str()),
                    ty: Self::rehydrate_type(ty, registry),
                }
            }
            FrozenBuiltinExpression::StructFieldAccess { base, name } => {
                Expression::StructFieldAccess {
                    base: Box::new(Self::rehydrate_expression(base, registry, component_roots)?),
                    name: SmolStr::new(name.as_str()),
                }
            }
            FrozenBuiltinExpression::Cast { from, to } => Expression::Cast {
                from: Box::new(Self::rehydrate_expression(from, registry, component_roots)?),
                to: Self::rehydrate_type(to, registry),
            },
            FrozenBuiltinExpression::CodeBlock(expressions) => Expression::CodeBlock(
                expressions
                    .iter()
                    .filter_map(|expr| Self::rehydrate_expression(expr, registry, component_roots))
                    .collect(),
            ),
            FrozenBuiltinExpression::FunctionCall { function, arguments } => {
                Expression::FunctionCall {
                    function: Self::rehydrate_callable(function, component_roots)?,
                    arguments: arguments
                        .iter()
                        .filter_map(|expr| {
                            Self::rehydrate_expression(expr, registry, component_roots)
                        })
                        .collect(),
                    source_location: None,
                }
            }
            FrozenBuiltinExpression::SelfAssignment { lhs, rhs, op } => {
                Expression::SelfAssignment {
                    lhs: Box::new(Self::rehydrate_expression(lhs, registry, component_roots)?),
                    rhs: Box::new(Self::rehydrate_expression(rhs, registry, component_roots)?),
                    op: *op,
                    node: None,
                }
            }
            FrozenBuiltinExpression::BinaryExpression { lhs, rhs, op } => {
                Expression::BinaryExpression {
                    lhs: Box::new(Self::rehydrate_expression(lhs, registry, component_roots)?),
                    rhs: Box::new(Self::rehydrate_expression(rhs, registry, component_roots)?),
                    op: *op,
                }
            }
            FrozenBuiltinExpression::UnaryOp { sub, op } => Expression::UnaryOp {
                sub: Box::new(Self::rehydrate_expression(sub, registry, component_roots)?),
                op: *op,
            },
            FrozenBuiltinExpression::Condition { condition, true_expr, false_expr } => {
                Expression::Condition {
                    condition: Box::new(Self::rehydrate_expression(
                        condition,
                        registry,
                        component_roots,
                    )?),
                    true_expr: Box::new(Self::rehydrate_expression(
                        true_expr,
                        registry,
                        component_roots,
                    )?),
                    false_expr: Box::new(Self::rehydrate_expression(
                        false_expr,
                        registry,
                        component_roots,
                    )?),
                }
            }
            FrozenBuiltinExpression::Array { element_ty, values } => Expression::Array {
                element_ty: Self::rehydrate_type(element_ty, registry),
                values: values
                    .iter()
                    .filter_map(|expr| Self::rehydrate_expression(expr, registry, component_roots))
                    .collect(),
            },
            FrozenBuiltinExpression::Struct { ty, values } => {
                let Type::Struct(ty) = Self::rehydrate_type(ty, registry) else {
                    return None;
                };
                Expression::Struct {
                    ty,
                    values: values
                        .iter()
                        .filter_map(|field| {
                            Some((
                                SmolStr::new(field.name.as_str()),
                                Self::rehydrate_expression(
                                    &field.expression,
                                    registry,
                                    component_roots,
                                )?,
                            ))
                        })
                        .collect(),
                }
            }
            FrozenBuiltinExpression::LinearGradient { angle, stops } => {
                Expression::LinearGradient {
                    angle: Box::new(Self::rehydrate_expression(angle, registry, component_roots)?),
                    stops: stops
                        .iter()
                        .filter_map(|stop| {
                            Some((
                                Self::rehydrate_expression(&stop.0, registry, component_roots)?,
                                Self::rehydrate_expression(&stop.1, registry, component_roots)?,
                            ))
                        })
                        .collect(),
                }
            }
            FrozenBuiltinExpression::RadialGradient { center, radius, stops } => {
                Expression::RadialGradient {
                    center: match center {
                        Some((x, y)) => Some((
                            Box::new(Self::rehydrate_expression(x, registry, component_roots)?),
                            Box::new(Self::rehydrate_expression(y, registry, component_roots)?),
                        )),
                        None => None,
                    },
                    radius: match radius {
                        Some(radius) => Some(Box::new(Self::rehydrate_expression(
                            radius,
                            registry,
                            component_roots,
                        )?)),
                        None => None,
                    },
                    stops: stops
                        .iter()
                        .filter_map(|stop| {
                            Some((
                                Self::rehydrate_expression(&stop.0, registry, component_roots)?,
                                Self::rehydrate_expression(&stop.1, registry, component_roots)?,
                            ))
                        })
                        .collect(),
                }
            }
            FrozenBuiltinExpression::ConicGradient { from_angle, center, stops } => {
                Expression::ConicGradient {
                    from_angle: Box::new(Self::rehydrate_expression(
                        from_angle,
                        registry,
                        component_roots,
                    )?),
                    center: match center {
                        Some((x, y)) => Some((
                            Box::new(Self::rehydrate_expression(x, registry, component_roots)?),
                            Box::new(Self::rehydrate_expression(y, registry, component_roots)?),
                        )),
                        None => None,
                    },
                    stops: stops
                        .iter()
                        .filter_map(|stop| {
                            Some((
                                Self::rehydrate_expression(&stop.0, registry, component_roots)?,
                                Self::rehydrate_expression(&stop.1, registry, component_roots)?,
                            ))
                        })
                        .collect(),
                }
            }
            FrozenBuiltinExpression::ReturnStatement(expression) => Expression::ReturnStatement(
                expression
                    .as_ref()
                    .and_then(|expr| Self::rehydrate_expression(expr, registry, component_roots))
                    .map(Box::new),
            ),
            FrozenBuiltinExpression::EnumerationValue { enumeration, value } => {
                let Type::Enumeration(enumeration) = registry.lookup(enumeration) else {
                    return None;
                };
                Expression::EnumerationValue(crate::langtype::EnumerationValue {
                    enumeration,
                    value: *value,
                })
            }
        })
    }

    fn rehydrate_callable(
        callable: &FrozenBuiltinCallable,
        component_roots: &HashMap<String, ElementRc>,
    ) -> Option<Callable> {
        Some(match callable {
            FrozenBuiltinCallable::Callback(reference) => {
                Callable::Callback(Self::rehydrate_named_reference(reference, component_roots)?)
            }
            FrozenBuiltinCallable::Function(reference) => {
                Callable::Function(Self::rehydrate_named_reference(reference, component_roots)?)
            }
            FrozenBuiltinCallable::Builtin(name) => {
                Callable::Builtin(frozen_builtin_function(name)?)
            }
        })
    }

    fn element_at_path(root: &ElementRc, path: &[usize]) -> Option<ElementRc> {
        let mut element = root.clone();
        for child_index in path {
            let child = element.borrow().children.get(*child_index)?.clone();
            element = child;
        }
        Some(element)
    }

    fn dummy_children_placeholder() -> crate::parser::syntax_nodes::ChildrenPlaceholder {
        let mut diag = crate::diagnostics::BuildDiagnostics::default();
        let node: crate::parser::syntax_nodes::Document =
            crate::parser::parse("component Dummy { @children }".into(), None, &mut diag).into();
        debug_assert!(!diag.has_errors());
        node.descendants()
            .find_map(crate::parser::syntax_nodes::ChildrenPlaceholder::new)
            .expect("dummy @children placeholder should parse")
    }

    fn dummy_struct_object_type(name: &str) -> crate::parser::syntax_nodes::ObjectType {
        let mut diag = crate::diagnostics::BuildDiagnostics::default();
        let node: crate::parser::syntax_nodes::Document =
            crate::parser::parse(format!("struct {name} {{}}").into(), None, &mut diag).into();
        debug_assert!(!diag.has_errors());
        node.descendants()
            .find_map(crate::parser::syntax_nodes::StructDeclaration::new)
            .map(|decl| decl.ObjectType())
            .expect("dummy struct declaration should parse")
    }

    fn rehydrate_element_skeleton(
        frozen_element: &FrozenBuiltinElement,
        element: &ElementRc,
        enclosing_component: Weak<Component>,
        registry: &TypeRegister,
        context: &FrozenBuiltinRehydrationContext,
    ) {
        let children = frozen_element
            .children
            .iter()
            .map(|child| {
                let child_element = Element::default().make_rc();
                Self::rehydrate_element_skeleton(
                    child,
                    &child_element,
                    enclosing_component.clone(),
                    registry,
                    context,
                );
                child_element
            })
            .collect();

        let mut property_declarations = std::collections::BTreeMap::new();
        for property in &frozen_element.property_declarations {
            property_declarations.insert(
                SmolStr::new(property.name.as_str()),
                PropertyDeclaration {
                    property_type: Self::rehydrate_type(&property.ty, registry),
                    visibility: Self::rehydrate_visibility(&property.visibility),
                    ..Default::default()
                },
            );
        }

        let mut element = element.borrow_mut();
        element.id = frozen_element.id.as_str().into();
        element.base_type = Self::rehydrate_element_base_type(
            &frozen_element.base_kind,
            &frozen_element.base_type,
            registry,
            context,
        );
        element.property_declarations = property_declarations;
        element.enclosing_component = enclosing_component;
        element.children = children;
    }

    fn rehydrate_element_base_type(
        kind: &str,
        name: &str,
        registry: &TypeRegister,
        context: &FrozenBuiltinRehydrationContext,
    ) -> ElementType {
        let base_type = match kind {
            "builtin" | "native" => context.lookup_builtin_or_native(name),
            "component" => registry
                .lookup_element(name)
                .unwrap_or_else(|_| context.lookup_builtin_or_native(name)),
            "global" => ElementType::Global,
            "interface" => ElementType::Interface,
            _ => registry
                .lookup_element(name)
                .unwrap_or_else(|_| context.lookup_builtin_or_native(name)),
        };
        base_type
    }

    fn rehydrate_type(name: &str, registry: &TypeRegister) -> Type {
        if let Some(function) = Self::rehydrate_function_type(name, "callback", registry) {
            return Type::Callback(Rc::new(function));
        }
        if let Some(function) = Self::rehydrate_function_type(name, "function", registry) {
            return Type::Function(Rc::new(function));
        }
        if let Some(inner) = name.strip_prefix('[').and_then(|name| name.strip_suffix(']')) {
            return Type::Array(Rc::new(Self::rehydrate_type(inner, registry)));
        }
        if let Some(enumeration) = name.strip_prefix("enum ") {
            return registry.lookup(enumeration);
        }

        let ty = registry.lookup(name);
        if ty == Type::Invalid {
            match name {
                "void" => Type::Void,
                _ => Type::Invalid,
            }
        } else {
            ty
        }
    }

    fn rehydrate_function_type(
        name: &str,
        kind: &str,
        registry: &TypeRegister,
    ) -> Option<Function> {
        let signature = name.strip_prefix(kind)?.trim_start();
        let (args, return_type) = signature.split_once("->")?;
        let args = args.trim();
        let args =
            args.strip_prefix('(').and_then(|args| args.strip_suffix(')')).unwrap_or(args).trim();
        let args = if args.is_empty() {
            Vec::new()
        } else {
            args.split(',').map(|arg| Self::rehydrate_type(arg.trim(), registry)).collect()
        };
        let arg_names = std::iter::repeat_n(SmolStr::default(), args.len()).collect();
        Some(Function {
            return_type: Self::rehydrate_type(return_type.trim(), registry),
            args,
            arg_names,
        })
    }

    fn rehydrate_visibility(visibility: &str) -> PropertyVisibility {
        match visibility {
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
}

struct FrozenBuiltinRehydrationContext {
    builtin_or_native_by_name: HashMap<String, ElementType>,
}

impl FrozenBuiltinRehydrationContext {
    fn new(registry: &TypeRegister) -> Self {
        let mut builtin_or_native_by_name = HashMap::new();
        for element_type in registry.all_elements().values() {
            let ElementType::Builtin(builtin) = element_type else {
                continue;
            };
            builtin_or_native_by_name.insert(builtin.name.to_string(), element_type.clone());
            builtin_or_native_by_name
                .insert(builtin.native_class.class_name.to_string(), element_type.clone());
        }

        let empty_type = registry.empty_type();
        if let ElementType::Builtin(builtin) = &empty_type {
            builtin_or_native_by_name.insert(builtin.name.to_string(), empty_type.clone());
            builtin_or_native_by_name
                .insert(builtin.native_class.class_name.to_string(), empty_type);
        }

        Self { builtin_or_native_by_name }
    }

    fn lookup_builtin_or_native(&self, name: &str) -> ElementType {
        self.builtin_or_native_by_name.get(name).cloned().unwrap_or_else(|| {
            let mut builtin = crate::langtype::BuiltinElement::new(Rc::new(
                crate::langtype::NativeClass::new(name),
            ));
            builtin.name = SmolStr::new(name);
            ElementType::Builtin(Rc::new(builtin))
        })
    }
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinDocument {
    pub(crate) path: String,
    pub(crate) imports: Vec<String>,
    pub(crate) exports: Vec<FrozenBuiltinExport>,
    pub(crate) components: Vec<FrozenBuiltinComponent>,
    pub(crate) inner_types: Vec<FrozenBuiltinType>,
    pub(crate) inner_component_count: usize,
    pub(crate) inner_type_count: usize,
}

#[derive(Clone, Debug)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) enum FrozenBuiltinType {
    Struct(FrozenBuiltinStruct),
    Enumeration { name: String, values: Vec<String>, default_value: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinExport {
    pub(crate) name: String,
    pub(crate) kind: FrozenBuiltinExportKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) enum FrozenBuiltinExportKind {
    Component,
    Type,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinComponent {
    pub(crate) id: String,
    pub(crate) root_element: FrozenBuiltinElement,
    pub(crate) child_insertion_point: Option<FrozenBuiltinChildrenInsertionPoint>,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinChildrenInsertionPoint {
    pub(crate) parent_path: Vec<usize>,
    pub(crate) insertion_index: usize,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinElement {
    pub(crate) id: String,
    pub(crate) base_kind: String,
    pub(crate) base_type: String,
    pub(crate) property_declarations: Vec<FrozenBuiltinPropertyDeclaration>,
    pub(crate) bindings: Vec<FrozenBuiltinBinding>,
    pub(crate) change_callbacks: Vec<String>,
    pub(crate) children: Vec<FrozenBuiltinElement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinPropertyDeclaration {
    pub(crate) name: String,
    pub(crate) ty: String,
    pub(crate) visibility: String,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinBinding {
    pub(crate) name: String,
    pub(crate) expression: FrozenBuiltinExpression,
    pub(crate) priority: i32,
    pub(crate) analysis_is_const: Option<bool>,
    pub(crate) two_way_bindings: Vec<FrozenBuiltinTwoWayBinding>,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinNamedReference {
    pub(crate) component: String,
    pub(crate) element_path: Vec<usize>,
    pub(crate) property: String,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinElementReference {
    pub(crate) component: String,
    pub(crate) element_path: Vec<usize>,
}

#[derive(Clone, Debug)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) enum FrozenBuiltinTwoWayBinding {
    Property { property: FrozenBuiltinNamedReference, field_access: Vec<String> },
    ModelData { repeated_element: FrozenBuiltinElementReference, field_access: Vec<String> },
}

#[derive(Clone, Debug)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) enum FrozenBuiltinCallable {
    Callback(FrozenBuiltinNamedReference),
    Function(FrozenBuiltinNamedReference),
    Builtin(String),
}

impl Default for FrozenBuiltinCallable {
    fn default() -> Self {
        Self::Builtin(String::new())
    }
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinStructExpressionField {
    pub(crate) name: String,
    pub(crate) expression: FrozenBuiltinExpression,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinGradientStop(
    pub(crate) FrozenBuiltinExpression,
    pub(crate) FrozenBuiltinExpression,
);

#[derive(Clone, Debug)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) enum FrozenBuiltinExpression {
    Invalid,
    StringLiteral(String),
    NumberLiteral {
        value: f64,
        unit: String,
    },
    BoolLiteral(bool),
    PropertyReference(FrozenBuiltinNamedReference),
    FunctionParameterReference {
        index: usize,
        ty: String,
    },
    StoreLocalVariable {
        name: String,
        value: Box<FrozenBuiltinExpression>,
    },
    ReadLocalVariable {
        name: String,
        ty: String,
    },
    StructFieldAccess {
        base: Box<FrozenBuiltinExpression>,
        name: String,
    },
    Cast {
        from: Box<FrozenBuiltinExpression>,
        to: String,
    },
    CodeBlock(Vec<FrozenBuiltinExpression>),
    FunctionCall {
        function: FrozenBuiltinCallable,
        arguments: Vec<FrozenBuiltinExpression>,
    },
    SelfAssignment {
        lhs: Box<FrozenBuiltinExpression>,
        rhs: Box<FrozenBuiltinExpression>,
        op: char,
    },
    BinaryExpression {
        lhs: Box<FrozenBuiltinExpression>,
        rhs: Box<FrozenBuiltinExpression>,
        op: char,
    },
    UnaryOp {
        sub: Box<FrozenBuiltinExpression>,
        op: char,
    },
    Condition {
        condition: Box<FrozenBuiltinExpression>,
        true_expr: Box<FrozenBuiltinExpression>,
        false_expr: Box<FrozenBuiltinExpression>,
    },
    Array {
        element_ty: String,
        values: Vec<FrozenBuiltinExpression>,
    },
    Struct {
        ty: String,
        values: Vec<FrozenBuiltinStructExpressionField>,
    },
    LinearGradient {
        angle: Box<FrozenBuiltinExpression>,
        stops: Vec<FrozenBuiltinGradientStop>,
    },
    RadialGradient {
        center: Option<(Box<FrozenBuiltinExpression>, Box<FrozenBuiltinExpression>)>,
        radius: Option<Box<FrozenBuiltinExpression>>,
        stops: Vec<FrozenBuiltinGradientStop>,
    },
    ConicGradient {
        from_angle: Box<FrozenBuiltinExpression>,
        center: Option<(Box<FrozenBuiltinExpression>, Box<FrozenBuiltinExpression>)>,
        stops: Vec<FrozenBuiltinGradientStop>,
    },
    ReturnStatement(Option<Box<FrozenBuiltinExpression>>),
    EnumerationValue {
        enumeration: String,
        value: usize,
    },
}

impl Default for FrozenBuiltinExpression {
    fn default() -> Self {
        Self::Invalid
    }
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinRegistry {
    pub(crate) types: Vec<String>,
    pub(crate) structs: Vec<FrozenBuiltinStruct>,
    pub(crate) elements: Vec<FrozenBuiltinRegistryElement>,
    pub(crate) supported_property_animation_types: Vec<String>,
    pub(crate) property_animation_type: String,
    pub(crate) property_animation_element: Option<FrozenBuiltinRegistryElement>,
    pub(crate) empty_type: String,
    pub(crate) empty_element: Option<FrozenBuiltinRegistryElement>,
    pub(crate) context_restricted_types: Vec<FrozenBuiltinContextRestriction>,
    pub(crate) expose_internal_types: bool,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinStruct {
    pub(crate) name: String,
    pub(crate) fields: Vec<FrozenBuiltinStructField>,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinStructField {
    pub(crate) name: String,
    pub(crate) ty: String,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinRegistryElement {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) component_root_id: String,
    pub(crate) component_root_base_kind: String,
    pub(crate) component_root_base_type: String,
    pub(crate) component_root_properties: Vec<FrozenBuiltinPropertyDeclaration>,
    pub(crate) native_class: String,
    pub(crate) native_class_hierarchy: Vec<FrozenBuiltinNativeClass>,
    pub(crate) property_count: usize,
    pub(crate) properties: Vec<FrozenBuiltinRegistryProperty>,
    pub(crate) native_properties: Vec<FrozenBuiltinRegistryProperty>,
    pub(crate) accepted_child_types: Vec<String>,
    pub(crate) additional_accept_self: bool,
    pub(crate) accepts_focus: bool,
    pub(crate) is_global: bool,
    pub(crate) is_internal: bool,
    pub(crate) is_non_item_type: bool,
    pub(crate) default_size_binding: String,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinNativeClass {
    pub(crate) name: String,
    pub(crate) properties: Vec<FrozenBuiltinRegistryProperty>,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinRegistryProperty {
    pub(crate) name: String,
    pub(crate) ty: String,
    pub(crate) visibility: String,
    pub(crate) default_kind: String,
    pub(crate) builtin_function: Option<String>,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(
    any(test, feature = "frozen-builtin-artifact-generation"),
    derive(serde::Serialize, serde::Deserialize)
)]
pub(crate) struct FrozenBuiltinContextRestriction {
    pub(crate) name: String,
    pub(crate) contexts: Vec<String>,
}

static FROZEN_BUILTIN_CACHE: OnceLock<Mutex<HashMap<FrozenBuiltinCacheKey, FrozenBuiltinLibrary>>> =
    OnceLock::new();

#[cfg(test)]
static GENERATED_BUILTIN_ARTIFACTS: OnceLock<Mutex<HashMap<FrozenBuiltinCacheKey, &'static [u8]>>> =
    OnceLock::new();

pub(crate) fn store(key: FrozenBuiltinCacheKey, library: FrozenBuiltinLibrary) {
    if library.documents.is_empty() {
        return;
    }

    FROZEN_BUILTIN_CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key)
        .or_insert(library);
}

pub(crate) fn get(key: &FrozenBuiltinCacheKey) -> Option<FrozenBuiltinLibrary> {
    FROZEN_BUILTIN_CACHE.get()?.lock().unwrap().get(key).cloned()
}

#[cfg(test)]
pub(crate) fn store_generated_artifact(key: FrozenBuiltinCacheKey, artifact: &'static [u8]) {
    if artifact.is_empty() {
        return;
    }

    GENERATED_BUILTIN_ARTIFACTS.get_or_init(Default::default).lock().unwrap().insert(key, artifact);
}

#[cfg(test)]
fn seeded_generated_artifact(key: &FrozenBuiltinCacheKey) -> Option<&'static [u8]> {
    GENERATED_BUILTIN_ARTIFACTS.get()?.lock().unwrap().get(key).copied()
}

#[cfg(not(test))]
fn seeded_generated_artifact(_key: &FrozenBuiltinCacheKey) -> Option<&'static [u8]> {
    None
}

pub(crate) fn generated_artifact(key: &FrozenBuiltinCacheKey) -> Option<&'static [u8]> {
    generated_builtin_artifacts::generated_artifact(key).or_else(|| seeded_generated_artifact(key))
}

pub(crate) fn generated_artifact_count() -> usize {
    generated_builtin_artifacts::artifact_count()
}

fn frozen_builtin_function(name: &str) -> Option<crate::expression_tree::BuiltinFunction> {
    use crate::expression_tree::BuiltinFunction;
    Some(match name {
        "GetWindowScaleFactor" => BuiltinFunction::GetWindowScaleFactor,
        "GetWindowDefaultFontSize" => BuiltinFunction::GetWindowDefaultFontSize,
        "AnimationTick" => BuiltinFunction::AnimationTick,
        "Debug" => BuiltinFunction::Debug,
        "Mod" => BuiltinFunction::Mod,
        "Round" => BuiltinFunction::Round,
        "Ceil" => BuiltinFunction::Ceil,
        "Floor" => BuiltinFunction::Floor,
        "Abs" => BuiltinFunction::Abs,
        "Sqrt" => BuiltinFunction::Sqrt,
        "Cos" => BuiltinFunction::Cos,
        "Sin" => BuiltinFunction::Sin,
        "Tan" => BuiltinFunction::Tan,
        "ACos" => BuiltinFunction::ACos,
        "ASin" => BuiltinFunction::ASin,
        "ATan" => BuiltinFunction::ATan,
        "ATan2" => BuiltinFunction::ATan2,
        "Log" => BuiltinFunction::Log,
        "Ln" => BuiltinFunction::Ln,
        "Pow" => BuiltinFunction::Pow,
        "Exp" => BuiltinFunction::Exp,
        "ToFixed" => BuiltinFunction::ToFixed,
        "ToPrecision" => BuiltinFunction::ToPrecision,
        "SetFocusItem" => BuiltinFunction::SetFocusItem,
        "ClearFocusItem" => BuiltinFunction::ClearFocusItem,
        "ShowPopupWindow" => BuiltinFunction::ShowPopupWindow,
        "ClosePopupWindow" => BuiltinFunction::ClosePopupWindow,
        "ShowPopupMenu" => BuiltinFunction::ShowPopupMenu,
        "ShowPopupMenuInternal" => BuiltinFunction::ShowPopupMenuInternal,
        "SetSelectionOffsets" => BuiltinFunction::SetSelectionOffsets,
        "ItemFontMetrics" => BuiltinFunction::ItemFontMetrics,
        "StringToFloat" => BuiltinFunction::StringToFloat,
        "StringIsFloat" => BuiltinFunction::StringIsFloat,
        "StringIsEmpty" => BuiltinFunction::StringIsEmpty,
        "StringCharacterCount" => BuiltinFunction::StringCharacterCount,
        "StringToLowercase" => BuiltinFunction::StringToLowercase,
        "StringToUppercase" => BuiltinFunction::StringToUppercase,
        "KeysToString" => BuiltinFunction::KeysToString,
        "ColorRgbaStruct" => BuiltinFunction::ColorRgbaStruct,
        "ColorHsvaStruct" => BuiltinFunction::ColorHsvaStruct,
        "ColorOklchStruct" => BuiltinFunction::ColorOklchStruct,
        "ColorBrighter" => BuiltinFunction::ColorBrighter,
        "ColorDarker" => BuiltinFunction::ColorDarker,
        "ColorTransparentize" => BuiltinFunction::ColorTransparentize,
        "ColorMix" => BuiltinFunction::ColorMix,
        "ColorWithAlpha" => BuiltinFunction::ColorWithAlpha,
        "ImageSize" => BuiltinFunction::ImageSize,
        "ArrayLength" => BuiltinFunction::ArrayLength,
        "Rgb" => BuiltinFunction::Rgb,
        "Hsv" => BuiltinFunction::Hsv,
        "Oklch" => BuiltinFunction::Oklch,
        "ColorScheme" => BuiltinFunction::ColorScheme,
        "AccentColor" => BuiltinFunction::AccentColor,
        "SupportsNativeMenuBar" => BuiltinFunction::SupportsNativeMenuBar,
        "SetupMenuBar" => BuiltinFunction::SetupMenuBar,
        "SetupSystemTrayIcon" => BuiltinFunction::SetupSystemTrayIcon,
        "Use24HourFormat" => BuiltinFunction::Use24HourFormat,
        "MonthDayCount" => BuiltinFunction::MonthDayCount,
        "MonthOffset" => BuiltinFunction::MonthOffset,
        "FormatDate" => BuiltinFunction::FormatDate,
        "DateNow" => BuiltinFunction::DateNow,
        "ValidDate" => BuiltinFunction::ValidDate,
        "ParseDate" => BuiltinFunction::ParseDate,
        "TextInputFocused" => BuiltinFunction::TextInputFocused,
        "SetTextInputFocused" => BuiltinFunction::SetTextInputFocused,
        "ImplicitLayoutInfo(Horizontal)" => {
            BuiltinFunction::ImplicitLayoutInfo(crate::layout::Orientation::Horizontal)
        }
        "ImplicitLayoutInfo(Vertical)" => {
            BuiltinFunction::ImplicitLayoutInfo(crate::layout::Orientation::Vertical)
        }
        "ItemAbsolutePosition" => BuiltinFunction::ItemAbsolutePosition,
        "RegisterCustomFontByPath" => BuiltinFunction::RegisterCustomFontByPath,
        "RegisterCustomFontByMemory" => BuiltinFunction::RegisterCustomFontByMemory,
        "RegisterBitmapFont" => BuiltinFunction::RegisterBitmapFont,
        "Translate" => BuiltinFunction::Translate,
        "UpdateTimers" => BuiltinFunction::UpdateTimers,
        "DetectOperatingSystem" => BuiltinFunction::DetectOperatingSystem,
        "StartTimer" => BuiltinFunction::StartTimer,
        "StopTimer" => BuiltinFunction::StopTimer,
        "RestartTimer" => BuiltinFunction::RestartTimer,
        "OpenUrl" => BuiltinFunction::OpenUrl,
        "MacosBringAllWindowsToFront" => BuiltinFunction::MacosBringAllWindowsToFront,
        "ParseMarkdown" => BuiltinFunction::ParseMarkdown,
        "StringToStyledText" => BuiltinFunction::StringToStyledText,
        "ColorToStyledText" => BuiltinFunction::ColorToStyledText,
        "DecimalSeparator" => BuiltinFunction::DecimalSeparator,
        _ => return None,
    })
}

#[cfg(any(test, feature = "frozen-builtin-artifact-generation"))]
pub(crate) fn render_generated_artifacts_module(
    entries: &[(FrozenBuiltinCacheKey, Vec<u8>)],
) -> String {
    let mut source = String::new();

    for (index, (_key, bytes)) in entries.iter().enumerate() {
        source.push_str(&format!("static ARTIFACT_{index}: &[u8] = &[\n"));
        for chunk in bytes.chunks(16) {
            source.push_str("    ");
            for byte in chunk {
                source.push_str(&format!("{byte},"));
            }
            source.push('\n');
        }
        source.push_str("];\n\n");
    }

    source.push_str(
        "pub(crate) fn generated_artifact(\n    key: &super::FrozenBuiltinCacheKey,\n) -> Option<&'static [u8]> {\n",
    );
    for (index, (key, _bytes)) in entries.iter().enumerate() {
        source.push_str("    if ");
        source.push_str(&render_generated_artifact_key_predicate(key));
        source.push_str(&format!(" {{\n        return Some(ARTIFACT_{index});\n    }}\n"));
    }
    source.push_str("    None\n}\n\n");
    source.push_str(&format!(
        "pub(crate) fn artifact_count() -> usize {{\n    {}\n}}\n",
        entries.len()
    ));

    source
}

#[cfg(any(test, feature = "frozen-builtin-artifact-generation"))]
pub(crate) fn render_generated_artifacts_include_module(
    entries: &[(FrozenBuiltinCacheKey, std::path::PathBuf)],
) -> String {
    let mut source = String::new();

    for (index, (_key, path)) in entries.iter().enumerate() {
        let file_name = path
            .file_name()
            .expect("generated artifact path should have a file name")
            .to_string_lossy();
        source.push_str(&format!(
            "static ARTIFACT_{index}: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{}\"));\n",
            file_name
        ));
    }
    source.push('\n');

    source.push_str(
        "pub(crate) fn generated_artifact(\n    key: &super::FrozenBuiltinCacheKey,\n) -> Option<&'static [u8]> {\n",
    );
    for (index, (key, _path)) in entries.iter().enumerate() {
        source.push_str("    if ");
        source.push_str(&render_generated_artifact_key_predicate(key));
        source.push_str(&format!(" {{\n        return Some(ARTIFACT_{index});\n    }}\n"));
    }
    source.push_str("    None\n}\n\n");
    source.push_str(&format!(
        "pub(crate) fn artifact_count() -> usize {{\n    {}\n}}\n",
        entries.len()
    ));

    source
}

#[cfg(any(test, feature = "frozen-builtin-artifact-generation"))]
fn render_generated_artifact_key_predicate(key: &FrozenBuiltinCacheKey) -> String {
    let translation_domain = match &key.translation_domain {
        Some(domain) => format!("Some({:?})", domain),
        None => "None".into(),
    };
    let default_translation_context = match key.default_translation_context {
        FrozenDefaultTranslationContext::ComponentName => "ComponentName",
        FrozenDefaultTranslationContext::None => "None",
    };

    format!(
        "key.resolved_style == {:?} && key.enable_experimental == {} && key.debug_hooks == {} && key.translation_domain.as_deref() == {} && matches!(key.default_translation_context, super::FrozenDefaultTranslationContext::{})",
        key.resolved_style,
        key.enable_experimental,
        key.debug_hooks,
        translation_domain,
        default_translation_context
    )
}
