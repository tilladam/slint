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

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::CompilerConfiguration;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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
pub(crate) struct FrozenBuiltinLibrary {
    pub(crate) documents: Vec<FrozenBuiltinDocument>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FrozenBuiltinDocument {
    pub(crate) path: String,
    pub(crate) imports: Vec<String>,
    pub(crate) exports: Vec<FrozenBuiltinExport>,
    pub(crate) inner_component_count: usize,
    pub(crate) inner_type_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrozenBuiltinExport {
    pub(crate) name: String,
    pub(crate) kind: FrozenBuiltinExportKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrozenBuiltinExportKind {
    Component,
    Type,
}

static FROZEN_BUILTIN_CACHE: OnceLock<Mutex<HashMap<FrozenBuiltinCacheKey, FrozenBuiltinLibrary>>> =
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
