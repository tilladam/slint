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
