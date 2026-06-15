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
use crate::langtype::{ElementType, Type};
use crate::object_tree::{Component, Element, ElementRc, PropertyDeclaration, PropertyVisibility};
use crate::typeregister::TypeRegister;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinLibrary {
    pub(crate) parent_registry: FrozenBuiltinRegistry,
    pub(crate) documents: Vec<FrozenBuiltinDocument>,
}

impl FrozenBuiltinLibrary {
    pub(crate) fn rehydrate_parent_registry(&self) -> Rc<RefCell<TypeRegister>> {
        TypeRegister::rehydrate_builtin_registry_shell(&self.parent_registry)
    }

    pub(crate) fn rehydrate_component_skeletons(
        &self,
        parent_registry: &Rc<RefCell<TypeRegister>>,
    ) -> TypeRegister {
        let mut registry = TypeRegister::new(parent_registry);
        let mut components = Vec::new();

        for frozen_component in self.documents.iter().flat_map(|doc| &doc.components) {
            let component = Rc::new(Component {
                id: frozen_component.id.as_str().into(),
                root_element: Element::default().make_rc(),
                ..Default::default()
            });
            registry.add(component.clone());
            components.push((component, frozen_component));
        }

        for (component, frozen_component) in components {
            Self::rehydrate_element_skeleton(
                &frozen_component.root_element,
                &component.root_element,
                Rc::downgrade(&component),
                &registry,
            );
        }

        registry
    }

    fn rehydrate_element_skeleton(
        frozen_element: &FrozenBuiltinElement,
        element: &ElementRc,
        enclosing_component: Weak<Component>,
        registry: &TypeRegister,
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
        element.base_type =
            registry.lookup_element(&frozen_element.base_type).unwrap_or(ElementType::Error);
        element.property_declarations = property_declarations;
        element.enclosing_component = enclosing_component;
        element.children = children;
    }

    fn rehydrate_type(name: &str, registry: &TypeRegister) -> Type {
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

#[derive(Clone, Debug, Default)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinDocument {
    pub(crate) path: String,
    pub(crate) imports: Vec<String>,
    pub(crate) exports: Vec<FrozenBuiltinExport>,
    pub(crate) components: Vec<FrozenBuiltinComponent>,
    pub(crate) inner_component_count: usize,
    pub(crate) inner_type_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinExport {
    pub(crate) name: String,
    pub(crate) kind: FrozenBuiltinExportKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum FrozenBuiltinExportKind {
    Component,
    Type,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinComponent {
    pub(crate) id: String,
    pub(crate) root_element: FrozenBuiltinElement,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinElement {
    pub(crate) id: String,
    pub(crate) base_type: String,
    pub(crate) property_declarations: Vec<FrozenBuiltinPropertyDeclaration>,
    pub(crate) bindings: Vec<String>,
    pub(crate) change_callbacks: Vec<String>,
    pub(crate) children: Vec<FrozenBuiltinElement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinPropertyDeclaration {
    pub(crate) name: String,
    pub(crate) ty: String,
    pub(crate) visibility: String,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinRegistry {
    pub(crate) types: Vec<String>,
    pub(crate) elements: Vec<FrozenBuiltinRegistryElement>,
    pub(crate) supported_property_animation_types: Vec<String>,
    pub(crate) property_animation_type: String,
    pub(crate) empty_type: String,
    pub(crate) context_restricted_types: Vec<FrozenBuiltinContextRestriction>,
    pub(crate) expose_internal_types: bool,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinRegistryElement {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) component_root_id: String,
    pub(crate) component_root_base_kind: String,
    pub(crate) component_root_base_type: String,
    pub(crate) component_root_properties: Vec<FrozenBuiltinPropertyDeclaration>,
    pub(crate) native_class: String,
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
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct FrozenBuiltinRegistryProperty {
    pub(crate) name: String,
    pub(crate) ty: String,
    pub(crate) visibility: String,
    pub(crate) default_kind: String,
    pub(crate) builtin_function: Option<String>,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
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
pub(crate) fn generated_artifact(key: &FrozenBuiltinCacheKey) -> Option<&'static [u8]> {
    GENERATED_BUILTIN_ARTIFACTS.get()?.lock().unwrap().get(key).copied()
}

#[cfg(not(test))]
pub(crate) fn generated_artifact(_key: &FrozenBuiltinCacheKey) -> Option<&'static [u8]> {
    None
}
