// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

// cSpell: ignore importident incdir splitn
use smol_str::{SmolStr, ToSmolStr};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use crate::diagnostics::{BuildDiagnostics, Diagnostic, Spanned};
use crate::expression_tree::Callable;
use crate::object_tree::{self, Document, ExportedName, Exports};
use crate::parser::{NodeOrToken, SyntaxKind, SyntaxToken, syntax_nodes};
use crate::typeregister::TypeRegister;
use crate::{CompilerConfiguration, expression_tree};
use crate::{fileaccess, langtype, layout, parser};
use core::future::Future;
use itertools::Itertools;

thread_local! {
    static BUILTIN_SEMANTIC_CACHE: RefCell<HashMap<crate::frozen_builtins::FrozenBuiltinCacheKey, TypeLoader>> =
        Default::default();
}

#[allow(clippy::large_enum_variant)]
enum LoadedDocument {
    Document(Document),
    /// A dependency of this file has changed, so we need to re-analyze it.
    /// The file contents have not changed, so we can keep the parsed CST around.
    Invalidated(syntax_nodes::Document),
}

/// Storage for a cache of all loaded documents
#[derive(Default)]
struct LoadedDocuments {
    /// maps from the canonical file name to the object_tree::Document.
    /// Also contains the error that occurred when parsing the document (and only the parse error, not further semantic errors)
    docs: HashMap<PathBuf, (LoadedDocument, Vec<crate::diagnostics::Diagnostic>)>,
    /// The .slint files that are currently being loaded, potentially asynchronously.
    /// When a task start loading a file, it will add an empty vector to this map, and
    /// the same task will remove the entry from the map when finished, and awake all
    /// wakers.
    currently_loading: HashMap<PathBuf, Vec<std::task::Waker>>,

    /// The dependencies of the currently loaded files.
    /// Maps all the files that depends directly on the key
    dependencies: HashMap<PathBuf, HashSet<PathBuf>>,
}

#[derive(Debug, Clone)]
pub enum ImportKind {
    /// `import {Foo, Bar} from "foo"`
    ImportList(syntax_nodes::ImportIdentifierList),
    /// `import "foo"` without an import list
    FileImport,
    /// re-export types, as per `export ... from "foo"``.
    ModuleReexport(syntax_nodes::ExportsList),
}

#[derive(Debug, Clone)]
pub struct LibraryInfo {
    pub name: String,
    pub package: String,
    pub module: Option<String>,
    pub exports: Vec<ExportedName>,
}

#[derive(Debug, Clone)]
pub struct ImportedTypes {
    pub import_uri_token: SyntaxToken,
    pub import_kind: ImportKind,
    pub file: String,

    /// `import {Foo, Bar} from "@Foo"` where Foo is an external
    /// library located in another crate
    pub library_info: Option<LibraryInfo>,
}

#[derive(Debug)]
pub struct ImportedName {
    // name of export to match in the other file
    pub external_name: SmolStr,
    // name to be used locally
    pub internal_name: SmolStr,
}

impl ImportedName {
    pub fn extract_imported_names(
        import_identifiers: &syntax_nodes::ImportIdentifierList,
    ) -> impl Iterator<Item = ImportedName> + '_ {
        import_identifiers.ImportIdentifier().map(Self::from_node)
    }

    pub fn from_node(importident: syntax_nodes::ImportIdentifier) -> Self {
        let external_name =
            parser::normalize_identifier(importident.ExternalName().text().to_smolstr().trim());

        let internal_name = match importident.InternalName() {
            Some(name_ident) => parser::normalize_identifier(name_ident.text().to_smolstr().trim()),
            None => external_name.clone(),
        };

        ImportedName { internal_name, external_name }
    }
}

/// This function makes a snapshot of the current state of the type loader.
/// This snapshot includes everything: Elements, Components, known types, ...
/// and can be used to roll back to earlier states in the compilation process.
///
/// One way this is used is to create a raw `TypeLoader` for analysis purposes
/// or to load a set of changes, see if those compile and then role back
///
/// The result may be `None` if the `TypeLoader` is actually in the process
/// of loading more documents and is `Some` `TypeLoader` with a copy off all
/// state connected with the original `TypeLoader`.
pub fn snapshot(type_loader: &TypeLoader) -> Option<TypeLoader> {
    let mut snapshotter = Snapshotter {
        component_map: HashMap::new(),
        element_map: HashMap::new(),
        type_register_map: HashMap::new(),
        keep_alive: Vec::new(),
        keep_alive_elements: Vec::new(),
    };
    snapshotter.snapshot_type_loader(type_loader)
}

/// This function makes a snapshot of the current state of the type loader.
/// This snapshot includes everything: Elements, Components, known types, ...
/// and can be used to roll back to earlier states in the compilation process.
///
/// One way this is used is to create a raw `TypeLoader` for analysis purposes
/// or to load a set of changes, see if those compile and then role back
///
/// The result may be `None` if the `TypeLoader` is actually in the process
/// of loading more documents and is `Some` `TypeLoader` with a copy off all
/// state connected with the original `TypeLoader`.
///
/// The Document will be added to the type_loader after it was snapshotted as well.
pub(crate) fn snapshot_with_extra_doc(
    type_loader: &TypeLoader,
    doc: &object_tree::Document,
) -> Option<TypeLoader> {
    let mut snapshotter = Snapshotter {
        component_map: HashMap::new(),
        element_map: HashMap::new(),
        type_register_map: HashMap::new(),
        keep_alive: Vec::new(),
        keep_alive_elements: Vec::new(),
    };
    let mut result = snapshotter.snapshot_type_loader(type_loader);

    snapshotter.create_document(doc);
    let new_doc = snapshotter.snapshot_document(doc);

    snapshotter.finalize();

    if let Some(doc_node) = &new_doc.node {
        let path = doc_node.source_file.path().to_path_buf();
        if let Some(r) = &mut result {
            r.all_documents.docs.insert(path, (LoadedDocument::Document(new_doc), Vec::new()));
        }
    }

    result
}

pub(crate) struct Snapshotter {
    component_map:
        HashMap<by_address::ByAddress<Rc<object_tree::Component>>, Weak<object_tree::Component>>,
    element_map:
        HashMap<by_address::ByAddress<object_tree::ElementRc>, Weak<RefCell<object_tree::Element>>>,
    type_register_map:
        HashMap<by_address::ByAddress<Rc<RefCell<TypeRegister>>>, Rc<RefCell<TypeRegister>>>,

    keep_alive: Vec<(Rc<object_tree::Component>, Rc<object_tree::Component>)>,
    keep_alive_elements: Vec<(object_tree::ElementRc, object_tree::ElementRc)>,
}

impl Snapshotter {
    fn snapshot_globals(&mut self, type_loader: &TypeLoader) {
        let registry = type_loader.global_type_registry.clone();
        registry
            .borrow()
            .all_elements()
            .values()
            .filter_map(|ty| match ty {
                langtype::ElementType::Component(c) if c.is_global() => Some(c),
                _ => None,
            })
            .for_each(|c| {
                self.create_component(c);
            });
    }

    fn finalize(&mut self) {
        let mut elements = std::mem::take(&mut self.keep_alive_elements);

        while !elements.is_empty() {
            for (s, t) in elements.iter_mut() {
                self.snapshot_element(s, &mut t.borrow_mut());
            }
            elements = std::mem::take(&mut self.keep_alive_elements);
        }
    }

    fn snapshot_type_loader(&mut self, type_loader: &TypeLoader) -> Option<TypeLoader> {
        self.snapshot_globals(type_loader);

        let all_documents = self.snapshot_loaded_documents(&type_loader.all_documents)?;

        self.finalize();

        Some(TypeLoader {
            all_documents,
            global_type_registry: self.snapshot_type_register(&type_loader.global_type_registry),
            compiler_config: type_loader.compiler_config.clone(),
            resolved_style: type_loader.resolved_style.clone(),
        })
    }

    pub(crate) fn snapshot_type_register(
        &mut self,
        type_register: &Rc<RefCell<TypeRegister>>,
    ) -> Rc<RefCell<TypeRegister>> {
        if let Some(r) = self.type_register_map.get(&by_address::ByAddress(type_register.clone())) {
            return r.clone();
        }

        let tr = Rc::new(RefCell::new(TypeRegister::default()));
        self.type_register_map.insert(by_address::ByAddress(type_register.clone()), tr.clone());

        *tr.borrow_mut() = self.snapshot_type_register_impl(type_register);

        tr
    }

    fn snapshot_type_register_impl(
        &mut self,
        type_register: &Rc<RefCell<TypeRegister>>,
    ) -> TypeRegister {
        type_register.borrow().snapshot(self)
    }

    fn snapshot_loaded_documents(
        &mut self,
        loaded_documents: &LoadedDocuments,
    ) -> Option<LoadedDocuments> {
        if !loaded_documents.currently_loading.is_empty() {
            return None;
        }

        loaded_documents.docs.values().for_each(|(d, _)| {
            if let LoadedDocument::Document(d) = d {
                self.create_document(d)
            }
        });

        Some(LoadedDocuments {
            docs: loaded_documents
                .docs
                .iter()
                .map(|(p, (d, err))| {
                    (
                        p.clone(),
                        (
                            match d {
                                LoadedDocument::Document(d) => {
                                    LoadedDocument::Document(self.snapshot_document(d))
                                }
                                LoadedDocument::Invalidated(d) => {
                                    LoadedDocument::Invalidated(d.clone())
                                }
                            },
                            err.clone(),
                        ),
                    )
                })
                .collect(),
            currently_loading: Default::default(),
            dependencies: Default::default(),
        })
    }

    fn create_document(&mut self, document: &object_tree::Document) {
        document.inner_components.iter().for_each(|ic| {
            let _ = self.create_component(ic);
        });
        if let Some(popup_menu_impl) = &document.popup_menu_impl {
            let _ = self.create_component(popup_menu_impl);
        }
    }

    fn snapshot_document(&mut self, document: &object_tree::Document) -> object_tree::Document {
        let inner_components = document
            .inner_components
            .iter()
            .map(|ic| {
                Weak::upgrade(&self.use_component(ic))
                    .expect("Components can get upgraded at this point")
            })
            .collect();
        let exports = document.exports.snapshot(self);

        object_tree::Document {
            node: document.node.clone(),
            inner_components,
            inner_types: document.inner_types.clone(),
            local_registry: document.local_registry.snapshot(self),
            custom_fonts: document.custom_fonts.clone(),
            imports: document.imports.clone(),
            exports,
            library_exports: document.library_exports.clone(),
            embedded_file_resources: document.embedded_file_resources.clone(),
            #[cfg(feature = "bundle-translations")]
            translation_builder: document.translation_builder.clone(),
            used_types: RefCell::new(self.snapshot_used_sub_types(&document.used_types.borrow())),
            popup_menu_impl: document.popup_menu_impl.as_ref().map(|p| {
                Weak::upgrade(&self.use_component(p))
                    .expect("Components can get upgraded at this point")
            }),
        }
    }

    pub(crate) fn create_component(
        &mut self,
        component: &Rc<object_tree::Component>,
    ) -> Rc<object_tree::Component> {
        let input_address = by_address::ByAddress(component.clone());

        let parent_element = if let Some(pe) = component.parent_element() {
            Rc::downgrade(&self.use_element(&pe))
        } else {
            Weak::default()
        };

        let result = Rc::new_cyclic(|weak| {
            self.component_map.insert(input_address, weak.clone());

            let root_element = self.create_element(&component.root_element);

            let optimized_elements = RefCell::new(
                component
                    .optimized_elements
                    .borrow()
                    .iter()
                    .map(|e| self.create_element(e))
                    .collect(),
            );

            let child_insertion_point =
                RefCell::new(component.child_insertion_point.borrow().clone());

            let popup_windows = RefCell::new(
                component
                    .popup_windows
                    .borrow()
                    .iter()
                    .map(|p| self.snapshot_popup_window(p))
                    .collect(),
            );
            let timers = RefCell::new(
                component.timers.borrow().iter().map(|p| self.snapshot_timer(p)).collect(),
            );
            let root_constraints = RefCell::new(
                self.snapshot_layout_constraints(&component.root_constraints.borrow()),
            );
            let menu_item_tree = component
                .menu_item_tree
                .borrow()
                .iter()
                .map(|it| self.create_component(it))
                .collect::<Vec<_>>()
                .into();
            object_tree::Component {
                node: component.node.clone(),
                id: component.id.clone(),
                child_insertion_point,
                exported_global_names: RefCell::new(
                    component.exported_global_names.borrow().clone(),
                ),
                used: component.used.clone(),
                init_code: RefCell::new(component.init_code.borrow().clone()),
                inherits_popup_window: std::cell::Cell::new(component.inherits_popup_window.get()),
                optimized_elements,
                parent_element: RefCell::new(parent_element),
                popup_windows,
                timers,
                menu_item_tree,
                private_properties: RefCell::new(component.private_properties.borrow().clone()),
                root_constraints,
                root_element,
                from_library: core::cell::Cell::new(false),
            }
        });
        self.keep_alive.push((component.clone(), result.clone()));
        result
    }

    pub(crate) fn use_component(
        &self,
        component: &Rc<object_tree::Component>,
    ) -> Weak<object_tree::Component> {
        self.component_map
            .get(&by_address::ByAddress(component.clone()))
            .expect("Component (Weak!) must exist at this point.")
            .clone()
    }

    pub(crate) fn create_element(
        &mut self,
        element: &object_tree::ElementRc,
    ) -> object_tree::ElementRc {
        let enclosing_component = if let Some(ec) = element.borrow().enclosing_component.upgrade() {
            self.use_component(&ec)
        } else {
            Weak::default()
        };

        let elem = element.borrow();

        let r = Rc::new_cyclic(|weak| {
            self.element_map.insert(by_address::ByAddress(element.clone()), weak.clone());

            let children = elem.children.iter().map(|c| self.create_element(c)).collect();

            RefCell::new(object_tree::Element {
                id: elem.id.clone(),
                enclosing_component,
                children,
                debug: elem.debug.clone(),
                ..Default::default()
            })
        });

        self.keep_alive_elements.push((element.clone(), r.clone()));
        r
    }

    fn create_and_snapshot_element(
        &mut self,
        element: &object_tree::ElementRc,
    ) -> object_tree::ElementRc {
        let target = self.create_element(element);
        self.snapshot_element(element, &mut target.borrow_mut());
        target
    }

    pub(crate) fn use_element(&self, element: &object_tree::ElementRc) -> object_tree::ElementRc {
        Weak::upgrade(
            &self
                .element_map
                .get(&by_address::ByAddress(element.clone()))
                .expect("Elements should have been known at this point")
                .clone(),
        )
        .expect("Must be able to upgrade here")
    }

    fn snapshot_element(
        &mut self,
        element: &object_tree::ElementRc,
        target_element: &mut object_tree::Element,
    ) {
        let elem = element.borrow();

        target_element.base_type = self.snapshot_element_type(&elem.base_type);

        target_element.transitions = elem
            .transitions
            .iter()
            .map(|t| object_tree::Transition {
                direction: t.direction,
                state_id: t.state_id.clone(),
                property_animations: t
                    .property_animations
                    .iter()
                    .map(|(nr, sl, el)| {
                        (nr.snapshot(self), sl.clone(), self.create_and_snapshot_element(el))
                    })
                    .collect(),
                node: t.node.clone(),
            })
            .collect();

        target_element.bindings = elem
            .bindings
            .iter()
            .map(|(k, v)| {
                let bm = v.borrow();
                let binding = self.snapshot_binding_expression(&bm);
                (k.clone(), RefCell::new(binding))
            })
            .collect();
        target_element.states = elem
            .states
            .iter()
            .map(|s| object_tree::State {
                id: s.id.clone(),
                condition: s.condition.clone(),
                property_changes: s
                    .property_changes
                    .iter()
                    .map(|(nr, expr, spc)| {
                        let nr = nr.snapshot(self);
                        let expr = self.snapshot_expression(expr);
                        (nr, expr, spc.clone())
                    })
                    .collect(),
            })
            .collect();
        target_element.repeated =
            elem.repeated.as_ref().map(|r| object_tree::RepeatedElementInfo {
                model: self.snapshot_expression(&r.model),
                model_data_id: r.model_data_id.clone(),
                index_id: r.index_id.clone(),
                is_conditional_element: r.is_conditional_element,
                is_listview: r.is_listview.as_ref().map(|lv| object_tree::ListViewInfo {
                    viewport_y: lv.viewport_y.snapshot(self),
                    viewport_height: lv.viewport_height.snapshot(self),
                    viewport_width: lv.viewport_width.snapshot(self),
                    listview_height: lv.listview_height.snapshot(self),
                    listview_width: lv.listview_width.snapshot(self),
                }),
            });

        target_element.accessibility_props = object_tree::AccessibilityProps(
            elem.accessibility_props.0.iter().map(|(k, v)| (k.clone(), v.snapshot(self))).collect(),
        );
        target_element.geometry_props =
            elem.geometry_props.as_ref().map(|gp| object_tree::GeometryProps {
                x: gp.x.snapshot(self),
                y: gp.y.snapshot(self),
                width: gp.width.snapshot(self),
                height: gp.height.snapshot(self),
            });
        target_element.property_declarations = elem
            .property_declarations
            .iter()
            .map(|(k, v)| {
                let decl = object_tree::PropertyDeclaration {
                    property_type: v.property_type.clone(),
                    node: v.node.clone(),
                    expose_in_public_api: v.expose_in_public_api,
                    is_alias: v.is_alias.as_ref().map(|a| a.snapshot(self)),
                    visibility: v.visibility,
                    pure: v.pure,
                };
                (k.clone(), decl)
            })
            .collect();
        target_element.layout_info_prop =
            elem.layout_info_prop.as_ref().map(|(n1, n2)| (n1.snapshot(self), n2.snapshot(self)));
        target_element.property_analysis = RefCell::new(elem.property_analysis.borrow().clone());

        target_element.change_callbacks = elem.change_callbacks.clone();
        target_element.child_of_layout = elem.child_of_layout;
        target_element.default_fill_parent = elem.default_fill_parent;
        target_element.has_popup_child = elem.has_popup_child;
        target_element.inline_depth = elem.inline_depth;
        target_element.is_component_placeholder = elem.is_component_placeholder;
        target_element.is_flickable_viewport = elem.is_flickable_viewport;
        target_element.is_legacy_syntax = elem.is_legacy_syntax;
        target_element.item_index = elem.item_index.clone();
        target_element.item_index_of_first_children = elem.item_index_of_first_children.clone();
        target_element.named_references = elem.named_references.snapshot(self);
    }

    fn snapshot_binding_expression(
        &mut self,
        binding_expression: &expression_tree::BindingExpression,
    ) -> expression_tree::BindingExpression {
        expression_tree::BindingExpression {
            expression: self.snapshot_expression(&binding_expression.expression),
            span: binding_expression.span.clone(),
            priority: binding_expression.priority,
            animation: binding_expression.animation.as_ref().map(|pa| match pa {
                object_tree::PropertyAnimation::Static(element) => {
                    object_tree::PropertyAnimation::Static(
                        self.create_and_snapshot_element(element),
                    )
                }
                object_tree::PropertyAnimation::Transition { state_ref, animations } => {
                    object_tree::PropertyAnimation::Transition {
                        state_ref: self.snapshot_expression(state_ref),
                        animations: animations
                            .iter()
                            .map(|tpa| object_tree::TransitionPropertyAnimation {
                                state_id: tpa.state_id,
                                direction: tpa.direction,
                                animation: self.create_and_snapshot_element(&tpa.animation),
                            })
                            .collect(),
                    }
                }
            }),
            analysis: binding_expression.analysis.as_ref().map(|a| {
                expression_tree::BindingAnalysis {
                    is_in_binding_loop: a.is_in_binding_loop.clone(),
                    is_const: a.is_const,
                    no_external_dependencies: a.no_external_dependencies,
                }
            }),
            two_way_bindings: binding_expression
                .two_way_bindings
                .iter()
                .map(|twb| match twb {
                    crate::expression_tree::TwoWayBinding::Property { property, field_access } => {
                        crate::expression_tree::TwoWayBinding::Property {
                            property: property.snapshot(self),
                            field_access: field_access.clone(),
                        }
                    }
                    crate::expression_tree::TwoWayBinding::ModelData {
                        repeated_element,
                        field_access,
                    } => crate::expression_tree::TwoWayBinding::ModelData {
                        repeated_element: repeated_element.clone(),
                        field_access: field_access.clone(),
                    },
                })
                .collect(),
        }
    }

    pub(crate) fn snapshot_element_type(
        &mut self,
        element_type: &langtype::ElementType,
    ) -> langtype::ElementType {
        // Components need to get adapted, the rest is fine I think...
        match element_type {
            langtype::ElementType::Component(component) => {
                // Some components that will get compiled out later...
                langtype::ElementType::Component(
                    Weak::upgrade(&self.use_component(component))
                        .expect("I can unwrap at this point"),
                )
            }
            _ => element_type.clone(),
        }
    }

    fn snapshot_used_sub_types(
        &mut self,
        used_types: &object_tree::UsedSubTypes,
    ) -> object_tree::UsedSubTypes {
        let globals = used_types
            .globals
            .iter()
            .map(|component| {
                Weak::upgrade(&self.use_component(component)).expect("Looking at a known component")
            })
            .collect();
        let structs_and_enums = used_types.structs_and_enums.clone();
        let sub_components = used_types
            .sub_components
            .iter()
            .map(|component| {
                Weak::upgrade(&self.use_component(component)).expect("Looking at a known component")
            })
            .collect();
        let library_types_imports = used_types.library_types_imports.clone();
        let library_global_imports = used_types.library_global_imports.clone();
        object_tree::UsedSubTypes {
            globals,
            structs_and_enums,
            sub_components,
            library_types_imports,
            library_global_imports,
        }
    }

    fn snapshot_popup_window(
        &mut self,
        popup_window: &object_tree::PopupWindow,
    ) -> object_tree::PopupWindow {
        object_tree::PopupWindow {
            component: Weak::upgrade(&self.use_component(&popup_window.component))
                .expect("Looking at a known component"),
            x: popup_window.x.snapshot(self),
            y: popup_window.y.snapshot(self),
            close_policy: popup_window.close_policy.clone(),
            parent_element: self.use_element(&popup_window.parent_element),
            is_tooltip: popup_window.is_tooltip,
        }
    }

    fn snapshot_timer(&mut self, timer: &object_tree::Timer) -> object_tree::Timer {
        object_tree::Timer {
            interval: timer.interval.snapshot(self),
            running: timer.running.snapshot(self),
            triggered: timer.triggered.snapshot(self),
            element: timer.element.clone(),
        }
    }

    fn snapshot_layout_constraints(
        &mut self,
        layout_constraints: &layout::LayoutConstraints,
    ) -> layout::LayoutConstraints {
        layout::LayoutConstraints {
            min_width: layout_constraints.min_width.as_ref().map(|lc| lc.snapshot(self)),
            max_width: layout_constraints.max_width.as_ref().map(|lc| lc.snapshot(self)),
            min_height: layout_constraints.min_height.as_ref().map(|lc| lc.snapshot(self)),
            max_height: layout_constraints.max_height.as_ref().map(|lc| lc.snapshot(self)),
            preferred_width: layout_constraints
                .preferred_width
                .as_ref()
                .map(|lc| lc.snapshot(self)),
            preferred_height: layout_constraints
                .preferred_height
                .as_ref()
                .map(|lc| lc.snapshot(self)),
            horizontal_stretch: layout_constraints
                .horizontal_stretch
                .as_ref()
                .map(|lc| lc.snapshot(self)),
            vertical_stretch: layout_constraints
                .vertical_stretch
                .as_ref()
                .map(|lc| lc.snapshot(self)),
            fixed_width: layout_constraints.fixed_width,
            fixed_height: layout_constraints.fixed_height,
        }
    }

    fn snapshot_expression(
        &mut self,
        expr: &expression_tree::Expression,
    ) -> expression_tree::Expression {
        use expression_tree::Expression;
        match expr {
            Expression::PropertyReference(nr) => Expression::PropertyReference(nr.snapshot(self)),
            Expression::ElementReference(el) => {
                Expression::ElementReference(if let Some(el) = el.upgrade() {
                    Rc::downgrade(&el)
                } else {
                    Weak::default()
                })
            }
            Expression::RepeaterIndexReference { element } => Expression::RepeaterIndexReference {
                element: if let Some(el) = element.upgrade() {
                    Rc::downgrade(&el)
                } else {
                    Weak::default()
                },
            },
            Expression::RepeaterModelReference { element } => Expression::RepeaterModelReference {
                element: if let Some(el) = element.upgrade() {
                    Rc::downgrade(&el)
                } else {
                    Weak::default()
                },
            },
            Expression::StoreLocalVariable { name, value } => Expression::StoreLocalVariable {
                name: name.clone(),
                value: Box::new(self.snapshot_expression(value)),
            },
            Expression::StructFieldAccess { base, name } => Expression::StructFieldAccess {
                base: Box::new(self.snapshot_expression(base)),
                name: name.clone(),
            },
            Expression::ArrayIndex { array, index } => Expression::ArrayIndex {
                array: Box::new(self.snapshot_expression(array)),
                index: Box::new(self.snapshot_expression(index)),
            },
            Expression::Cast { from, to } => {
                Expression::Cast { from: Box::new(self.snapshot_expression(from)), to: to.clone() }
            }
            Expression::CodeBlock(exprs) => {
                Expression::CodeBlock(exprs.iter().map(|e| self.snapshot_expression(e)).collect())
            }
            Expression::FunctionCall { function, arguments, source_location } => {
                Expression::FunctionCall {
                    function: match function {
                        Callable::Callback(nr) => Callable::Callback(nr.snapshot(self)),
                        Callable::Function(nr) => Callable::Function(nr.snapshot(self)),
                        Callable::Builtin(b) => Callable::Builtin(b.clone()),
                    },
                    arguments: arguments.iter().map(|e| self.snapshot_expression(e)).collect(),
                    source_location: source_location.clone(),
                }
            }
            Expression::SelfAssignment { lhs, rhs, op, node } => Expression::SelfAssignment {
                lhs: Box::new(self.snapshot_expression(lhs)),
                rhs: Box::new(self.snapshot_expression(rhs)),
                op: *op,
                node: node.clone(),
            },
            Expression::BinaryExpression { lhs, rhs, op } => Expression::BinaryExpression {
                lhs: Box::new(self.snapshot_expression(lhs)),
                rhs: Box::new(self.snapshot_expression(rhs)),
                op: *op,
            },
            Expression::UnaryOp { sub, op } => {
                Expression::UnaryOp { sub: Box::new(self.snapshot_expression(sub)), op: *op }
            }
            Expression::Condition { condition, true_expr, false_expr } => Expression::Condition {
                condition: Box::new(self.snapshot_expression(condition)),
                true_expr: Box::new(self.snapshot_expression(true_expr)),
                false_expr: Box::new(self.snapshot_expression(false_expr)),
            },
            Expression::Array { element_ty, values } => Expression::Array {
                element_ty: element_ty.clone(),
                values: values.iter().map(|e| self.snapshot_expression(e)).collect(),
            },
            Expression::Struct { ty, values } => Expression::Struct {
                ty: ty.clone(),
                values: values
                    .iter()
                    .map(|(k, v)| (k.clone(), self.snapshot_expression(v)))
                    .collect(),
            },
            Expression::PathData(path) => Expression::PathData(match path {
                expression_tree::Path::Elements(path_elements) => expression_tree::Path::Elements(
                    path_elements
                        .iter()
                        .map(|p| {
                            expression_tree::PathElement {
                                element_type: p.element_type.clone(), // builtin should be OK to clone
                                bindings: p
                                    .bindings
                                    .iter()
                                    .map(|(k, v)| {
                                        (
                                            k.clone(),
                                            RefCell::new(
                                                self.snapshot_binding_expression(&v.borrow()),
                                            ),
                                        )
                                    })
                                    .collect(),
                            }
                        })
                        .collect(),
                ),
                expression_tree::Path::Events(ex1, ex2) => expression_tree::Path::Events(
                    ex1.iter().map(|e| self.snapshot_expression(e)).collect(),
                    ex2.iter().map(|e| self.snapshot_expression(e)).collect(),
                ),
                expression_tree::Path::Commands(ex) => {
                    expression_tree::Path::Commands(Box::new(self.snapshot_expression(ex)))
                }
            }),
            Expression::LinearGradient { angle, stops } => Expression::LinearGradient {
                angle: Box::new(self.snapshot_expression(angle)),
                stops: stops
                    .iter()
                    .map(|(e1, e2)| (self.snapshot_expression(e1), self.snapshot_expression(e2)))
                    .collect(),
            },
            Expression::RadialGradient { center, radius, stops } => Expression::RadialGradient {
                center: center.as_ref().map(|(cx, cy)| {
                    (Box::new(self.snapshot_expression(cx)), Box::new(self.snapshot_expression(cy)))
                }),
                radius: radius.as_ref().map(|r| Box::new(self.snapshot_expression(r))),
                stops: stops
                    .iter()
                    .map(|(e1, e2)| (self.snapshot_expression(e1), self.snapshot_expression(e2)))
                    .collect(),
            },
            Expression::ConicGradient { from_angle, center, stops } => Expression::ConicGradient {
                from_angle: Box::new(self.snapshot_expression(from_angle)),
                center: center.as_ref().map(|(cx, cy)| {
                    (Box::new(self.snapshot_expression(cx)), Box::new(self.snapshot_expression(cy)))
                }),
                stops: stops
                    .iter()
                    .map(|(e1, e2)| (self.snapshot_expression(e1), self.snapshot_expression(e2)))
                    .collect(),
            },
            Expression::ReturnStatement(expr) => Expression::ReturnStatement(
                expr.as_ref().map(|e| Box::new(self.snapshot_expression(e))),
            ),
            Expression::LayoutCacheAccess {
                layout_cache_prop,
                index,
                repeater_index,
                entries_per_item,
            } => Expression::LayoutCacheAccess {
                layout_cache_prop: layout_cache_prop.snapshot(self),
                index: *index,
                repeater_index: repeater_index
                    .as_ref()
                    .map(|e| Box::new(self.snapshot_expression(e))),
                entries_per_item: *entries_per_item,
            },
            Expression::GridRepeaterCacheAccess {
                layout_cache_prop,
                index,
                repeater_index,
                stride,
                child_offset,
                inner_repeater_index,
                entries_per_item,
            } => Expression::GridRepeaterCacheAccess {
                layout_cache_prop: layout_cache_prop.snapshot(self),
                index: *index,
                repeater_index: Box::new(self.snapshot_expression(repeater_index)),
                stride: Box::new(self.snapshot_expression(stride)),
                child_offset: *child_offset,
                inner_repeater_index: inner_repeater_index
                    .as_ref()
                    .map(|e| Box::new(self.snapshot_expression(e))),
                entries_per_item: *entries_per_item,
            },
            Expression::MinMax { ty, op, lhs, rhs } => Expression::MinMax {
                ty: ty.clone(),
                lhs: Box::new(self.snapshot_expression(lhs)),
                rhs: Box::new(self.snapshot_expression(rhs)),
                op: *op,
            },
            _ => expr.clone(),
        }
    }
}

pub struct TypeLoader {
    pub global_type_registry: Rc<RefCell<TypeRegister>>,
    pub compiler_config: CompilerConfiguration,
    /// The style that was specified in the compiler configuration, but resolved. So "native" for example is resolved to the concrete
    /// style.
    pub resolved_style: String,
    all_documents: LoadedDocuments,
}

struct BorrowedTypeLoader<'a> {
    tl: &'a mut TypeLoader,
    diag: &'a mut BuildDiagnostics,
}

#[cfg(feature = "frozen-builtin-artifacts")]
fn sanitize_artifact_file_stem(style: &str) -> String {
    style
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '_' })
        .collect()
}

#[cfg(feature = "frozen-builtin-artifacts")]
fn render_frozen_builtin_artifact_manifest(
    module_path: &Path,
    entries: &[(String, crate::frozen_builtins::FrozenBuiltinCacheKey, PathBuf, u64)],
) -> String {
    let mut manifest = String::from("schema=frozen-builtin-artifacts-v0\n");
    manifest.push_str(&format!(
        "artifact_schema_version={}\n",
        crate::frozen_builtins::FROZEN_BUILTIN_SCHEMA_VERSION
    ));
    manifest.push_str(&format!("module={}\n", module_path.display()));
    manifest.push_str(&format!("artifact_count={}\n", entries.len()));

    for (index, (style, key, path, size)) in entries.iter().enumerate() {
        manifest.push_str(&format!("\n[artifact.{index}]\n"));
        manifest.push_str(&format!("style={style}\n"));
        manifest.push_str(&format!("path={}\n", path.display()));
        manifest.push_str(&format!("bytes={size}\n"));
        manifest.push_str(&format!("key.resolved_style={}\n", key.resolved_style));
        manifest.push_str(&format!("key.enable_experimental={}\n", key.enable_experimental));
        manifest.push_str(&format!("key.debug_hooks={}\n", key.debug_hooks));
        manifest.push_str(&format!(
            "key.translation_domain={}\n",
            key.translation_domain.as_deref().unwrap_or("")
        ));
        manifest.push_str(&format!(
            "key.default_translation_context={:?}\n",
            key.default_translation_context
        ));
    }

    manifest
}

impl TypeLoader {
    #[cfg(feature = "frozen-builtin-artifacts")]
    pub fn generate_frozen_builtin_artifact_files(
        styles: &[String],
        artifact_dir: &Path,
    ) -> Result<PathBuf, String> {
        std::fs::create_dir_all(artifact_dir).map_err(|err| {
            format!("failed to create artifact directory {}: {err}", artifact_dir.display())
        })?;

        let mut styles = styles.to_vec();
        styles.sort();
        styles.dedup();

        let mut entries = Vec::new();
        let mut manifest_entries = Vec::new();
        for style in &styles {
            let mut compiler_config =
                CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
            compiler_config.style = Some(style.into());
            let source = r#"import { Button } from "std-widgets.slint";
export component Test inherits Button {}"#;

            let mut parse_diags = BuildDiagnostics::default();
            let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
            let (_doc, compile_diags, loader) = spin_on::spin_on(crate::compile_syntax_node(
                doc_node,
                parse_diags,
                compiler_config.clone(),
            ));
            if compile_diags.has_errors() {
                return Err(format!(
                    "failed to generate frozen builtin artifact for style {style}: {:?}",
                    compile_diags.to_string_vec()
                ));
            }

            let key = Self::builtin_semantic_cache_key_for(&compiler_config, style)
                .ok_or_else(|| format!("style {style} is not cacheable"))?;
            let artifact = postcard::to_allocvec(&loader.freeze_builtin_semantic_metadata())
                .map_err(|err| {
                    format!("failed to encode frozen artifact for style {style}: {err}")
                })?;
            let artifact_path =
                artifact_dir.join(format!("{}.postcard", sanitize_artifact_file_stem(style)));
            std::fs::write(&artifact_path, artifact).map_err(|err| {
                format!("failed to write artifact {}: {err}", artifact_path.display())
            })?;
            let artifact_size = std::fs::metadata(&artifact_path)
                .map_err(|err| {
                    format!("failed to stat artifact {}: {err}", artifact_path.display())
                })?
                .len();
            manifest_entries.push((
                style.clone(),
                key.clone(),
                artifact_path.clone(),
                artifact_size,
            ));
            entries.push((key, artifact_path));
        }

        let module_source =
            crate::frozen_builtins::render_generated_artifacts_include_module(&entries);
        let module_path = artifact_dir.join("frozen_builtin_artifacts.rs");
        std::fs::write(&module_path, module_source)
            .map_err(|err| format!("failed to write module {}: {err}", module_path.display()))?;
        let manifest_path = artifact_dir.join("frozen_builtin_artifacts.manifest");
        std::fs::write(
            &manifest_path,
            render_frozen_builtin_artifact_manifest(&module_path, &manifest_entries),
        )
        .map_err(|err| format!("failed to write manifest {}: {err}", manifest_path.display()))?;
        Ok(module_path)
    }

    pub fn new(compiler_config: CompilerConfiguration, diag: &mut BuildDiagnostics) -> Self {
        let mut style = compiler_config.style.clone().unwrap_or_else(|| "fluent".into());

        if style == "native" {
            style = get_native_style(&mut diag.all_loaded_files);
        }

        if let Some(cached) = Self::restore_builtin_semantic_cache_for(&compiler_config, &style) {
            return cached;
        }
        if let Some(cached) = Self::restore_frozen_builtin_cache_for(&compiler_config, &style) {
            return cached;
        }
        if let Some(cached) = Self::restore_generated_builtin_artifact_for(&compiler_config, &style)
        {
            return cached;
        }

        let myself = Self {
            global_type_registry: if compiler_config.enable_experimental {
                crate::typeregister::TypeRegister::builtin_experimental()
            } else {
                crate::typeregister::TypeRegister::builtin()
            },
            compiler_config,
            resolved_style: style.clone(),
            all_documents: Default::default(),
        };

        let mut known_styles = fileaccess::styles();
        known_styles.push("native");
        if !known_styles.contains(&style.as_ref())
            && myself
                .find_file_in_include_path(None, &format!("{style}/std-widgets.slint"))
                .is_none()
        {
            diag.push_diagnostic_with_span(
                format!(
                    "Style {} is not known. Use one of the builtin styles [{}] or make sure your custom style is found in the include directories",
                    &style,
                    known_styles.join(", ")
                ),
                Default::default(),
                crate::diagnostics::DiagnosticLevel::Error,
            );
        }

        myself
    }

    fn builtin_semantic_cache_key(&self) -> Option<crate::frozen_builtins::FrozenBuiltinCacheKey> {
        Self::builtin_semantic_cache_key_for(&self.compiler_config, &self.resolved_style)
    }

    fn builtin_semantic_cache_key_for(
        compiler_config: &CompilerConfiguration,
        resolved_style: &str,
    ) -> Option<crate::frozen_builtins::FrozenBuiltinCacheKey> {
        crate::frozen_builtins::FrozenBuiltinCacheKey::from_config(compiler_config, resolved_style)
    }

    fn restore_builtin_semantic_cache_for(
        compiler_config: &CompilerConfiguration,
        resolved_style: &str,
    ) -> Option<Self> {
        let key = Self::builtin_semantic_cache_key_for(compiler_config, resolved_style)?;
        BUILTIN_SEMANTIC_CACHE.with(|cache| {
            let cache = cache.borrow();
            let cached = cache.get(&key)?;
            let mut restored = snapshot(cached)?;
            restored.compiler_config = compiler_config.clone();
            restored.resolved_style = resolved_style.into();
            Some(restored)
        })
    }

    fn restore_frozen_builtin_cache_for(
        compiler_config: &CompilerConfiguration,
        resolved_style: &str,
    ) -> Option<Self> {
        let frozen = Self::cached_frozen_builtin_metadata(compiler_config, resolved_style)?;
        Self::rehydrate_frozen_builtin_artifact(
            compiler_config.clone(),
            resolved_style.into(),
            &frozen,
        )
    }

    fn restore_generated_builtin_artifact_for(
        compiler_config: &CompilerConfiguration,
        resolved_style: &str,
    ) -> Option<Self> {
        let key = Self::builtin_semantic_cache_key_for(compiler_config, resolved_style)?;
        let artifact = crate::frozen_builtins::generated_artifact(&key)?;
        Self::rehydrate_generated_builtin_artifact_bytes(
            compiler_config.clone(),
            resolved_style.into(),
            artifact,
        )
    }

    #[cfg(any(test, feature = "frozen-builtin-artifacts"))]
    fn rehydrate_generated_builtin_artifact_bytes(
        compiler_config: CompilerConfiguration,
        resolved_style: String,
        artifact: &'static [u8],
    ) -> Option<Self> {
        let frozen =
            postcard::from_bytes::<crate::frozen_builtins::FrozenBuiltinLibrary>(artifact).ok()?;
        Self::rehydrate_frozen_builtin_artifact(compiler_config, resolved_style, &frozen)
    }

    #[cfg(not(any(test, feature = "frozen-builtin-artifacts")))]
    fn rehydrate_generated_builtin_artifact_bytes(
        _compiler_config: CompilerConfiguration,
        _resolved_style: String,
        _artifact: &'static [u8],
    ) -> Option<Self> {
        None
    }

    fn rehydrate_frozen_builtin_artifact(
        compiler_config: CompilerConfiguration,
        resolved_style: String,
        frozen: &crate::frozen_builtins::FrozenBuiltinLibrary,
    ) -> Option<Self> {
        if !frozen.is_supported_schema_version() {
            return None;
        }

        let global_type_registry = frozen.rehydrate_parent_registry();
        let builtin_registry = frozen.rehydrate_component_skeletons(&global_type_registry);
        let mut all_documents = LoadedDocuments::default();

        for frozen_document in &frozen.documents {
            let path = PathBuf::from(&frozen_document.path);
            let mut parse_diags = BuildDiagnostics::default();
            let doc_node: syntax_nodes::Document =
                crate::parser::parse(String::new(), Some(&path), &mut parse_diags).into();
            let name_ident: crate::parser::SyntaxNode = doc_node.clone().into();

            let mut local_registry = TypeRegister::new(&global_type_registry);
            let inner_components = frozen_document
                .components
                .iter()
                .filter_map(|component| match builtin_registry.lookup_element(&component.id) {
                    Ok(langtype::ElementType::Component(component)) => {
                        local_registry.add(component.clone());
                        Some(component)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();

            let mut exports = Exports::default();
            let exports_to_add = frozen_document.exports.iter().filter_map(|export| {
                let exported_name = ExportedName {
                    name: SmolStr::new(export.name.as_str()),
                    name_ident: name_ident.clone(),
                };
                let exported = match export.kind {
                    crate::frozen_builtins::FrozenBuiltinExportKind::Component => {
                        match builtin_registry.lookup_element(&export.name) {
                            Ok(langtype::ElementType::Component(component)) => {
                                itertools::Either::Left(component)
                            }
                            _ => return None,
                        }
                    }
                    crate::frozen_builtins::FrozenBuiltinExportKind::Type => {
                        let ty = builtin_registry.lookup(&export.name);
                        if ty == langtype::Type::Invalid {
                            return None;
                        }
                        itertools::Either::Right(ty)
                    }
                };
                Some((exported_name, exported))
            });
            let mut export_diags = BuildDiagnostics::default();
            exports.add_reexports(exports_to_add, &mut export_diags);

            let document = Document {
                node: Some(doc_node),
                inner_components,
                local_registry,
                exports,
                ..Default::default()
            };
            all_documents.docs.insert(path, (LoadedDocument::Document(document), Vec::new()));
        }

        Some(Self { global_type_registry, compiler_config, resolved_style, all_documents })
    }

    pub(crate) fn store_builtin_semantic_cache(&self) {
        let Some(key) = self.builtin_semantic_cache_key() else { return };

        self.store_frozen_builtin_cache();

        let Some(mut cached) = snapshot(self) else { return };
        cached.retain_only_builtin_documents();

        if cached.all_documents.docs.is_empty() {
            return;
        }

        BUILTIN_SEMANTIC_CACHE.with(|cache| {
            cache.borrow_mut().entry(key).or_insert(cached);
        });
    }

    pub(crate) fn store_frozen_builtin_cache(&self) {
        let Some(key) = self.builtin_semantic_cache_key() else { return };
        crate::frozen_builtins::store(key, self.freeze_builtin_semantic_metadata());
    }

    #[allow(dead_code)]
    pub(crate) fn cached_frozen_builtin_metadata(
        compiler_config: &CompilerConfiguration,
        resolved_style: &str,
    ) -> Option<crate::frozen_builtins::FrozenBuiltinLibrary> {
        let key = Self::builtin_semantic_cache_key_for(compiler_config, resolved_style)?;
        crate::frozen_builtins::get(&key)
    }

    fn retain_only_builtin_documents(&mut self) {
        self.all_documents.docs.retain(|path, _| path.starts_with("builtin:/"));
        self.all_documents.dependencies.retain(|path, dependents| {
            if !path.starts_with("builtin:/") {
                return false;
            }
            dependents.retain(|path| path.starts_with("builtin:/"));
            true
        });
        self.all_documents.currently_loading.clear();
    }

    #[allow(dead_code)]
    pub(crate) fn freeze_builtin_semantic_metadata(
        &self,
    ) -> crate::frozen_builtins::FrozenBuiltinLibrary {
        let mut documents = self
            .all_documents
            .docs
            .iter()
            .filter_map(|(path, (document, _diagnostics))| {
                if !path.starts_with("builtin:/") {
                    return None;
                }
                let LoadedDocument::Document(document) = document else {
                    return None;
                };

                Some(crate::frozen_builtins::FrozenBuiltinDocument {
                    path: path.to_string_lossy().into_owned(),
                    imports: document.imports.iter().map(|import| import.file.clone()).collect(),
                    exports: document
                        .exports
                        .iter()
                        .map(|(exported_name, component_or_type)| {
                            let kind = match component_or_type {
                                itertools::Either::Left(_) => {
                                    crate::frozen_builtins::FrozenBuiltinExportKind::Component
                                }
                                itertools::Either::Right(_) => {
                                    crate::frozen_builtins::FrozenBuiltinExportKind::Type
                                }
                            };
                            crate::frozen_builtins::FrozenBuiltinExport {
                                name: exported_name.name.to_string(),
                                kind,
                            }
                        })
                        .collect(),
                    components: document
                        .inner_components
                        .iter()
                        .map(Self::freeze_builtin_component_skeleton)
                        .collect(),
                    inner_component_count: document.inner_components.len(),
                    inner_type_count: document.inner_types.len(),
                })
            })
            .collect::<Vec<_>>();
        documents.sort_by(|lhs, rhs| lhs.path.cmp(&rhs.path));
        crate::frozen_builtins::FrozenBuiltinLibrary {
            schema_version: crate::frozen_builtins::FROZEN_BUILTIN_SCHEMA_VERSION,
            parent_registry: self.global_type_registry.borrow().freeze_builtin_registry_metadata(),
            documents,
        }
    }

    fn freeze_builtin_component_skeleton(
        component: &Rc<object_tree::Component>,
    ) -> crate::frozen_builtins::FrozenBuiltinComponent {
        crate::frozen_builtins::FrozenBuiltinComponent {
            id: component.id.to_string(),
            root_element: Self::freeze_builtin_element_skeleton(&component.root_element),
        }
    }

    fn freeze_builtin_element_skeleton(
        element: &object_tree::ElementRc,
    ) -> crate::frozen_builtins::FrozenBuiltinElement {
        let element = element.borrow();
        let base_type = element
            .base_type
            .type_name()
            .map(str::to_string)
            .unwrap_or_else(|| element.base_type.to_string());
        let mut property_declarations = element
            .property_declarations
            .iter()
            .map(|(name, declaration)| crate::frozen_builtins::FrozenBuiltinPropertyDeclaration {
                name: name.to_string(),
                ty: declaration.property_type.to_string(),
                visibility: declaration.visibility.to_string(),
            })
            .collect::<Vec<_>>();
        property_declarations.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

        let mut bindings = element.bindings.keys().map(ToString::to_string).collect::<Vec<_>>();
        bindings.sort();

        let mut change_callbacks =
            element.change_callbacks.keys().map(ToString::to_string).collect::<Vec<_>>();
        change_callbacks.sort();

        crate::frozen_builtins::FrozenBuiltinElement {
            id: element.id.to_string(),
            base_type,
            property_declarations,
            bindings,
            change_callbacks,
            children: element.children.iter().map(Self::freeze_builtin_element_skeleton).collect(),
        }
    }

    /// Drop a document from the TypeLoader and invalidate all of its dependencies.
    /// Returns the list of all (transitive) dependencies.
    ///
    /// This forces the compiler to entirely reload the document from scratch.
    /// To only cause a re-analyze, but not a reparse, use [Self::invalidate_document]
    pub fn drop_document(&mut self, path: &Path) -> Result<HashSet<PathBuf>, std::io::Error> {
        let dependencies = self.invalidate_document(path);
        self.all_documents.docs.remove(path);

        if self.all_documents.currently_loading.contains_key(path) {
            Err(std::io::Error::new(ErrorKind::InvalidInput, format!("{path:?} is still loading")))
        } else {
            Ok(dependencies)
        }
    }

    /// Invalidate a document and all its dependencies.
    ///
    /// This will keep the CST of the document in cache, but mark that it needs to be re-analyzed
    /// to reconstruct its types.
    ///
    /// To entirely forget a document and cause a complete re-parse, use [Self::drop_document].
    pub fn invalidate_document(&mut self, path: &Path) -> HashSet<PathBuf> {
        if let Some((d, _)) = self.all_documents.docs.get_mut(path) {
            if let LoadedDocument::Document(doc) = d {
                for import in &doc.imports {
                    self.all_documents
                        .dependencies
                        .entry(Path::new(&import.file).into())
                        .or_default()
                        .remove(path);
                }
                match doc.node.take() {
                    None => {
                        self.all_documents.docs.remove(path);
                    }
                    Some(n) => {
                        *d = LoadedDocument::Invalidated(n);
                    }
                };
            } else {
                return HashSet::new();
            }
        } else {
            // If a document is not in the TypeLoader, it may still have dependencies,
            // as another document may have tried to import it, but it failed (e.g. the file didn't exist).
            // So still invalidate all dependencies, even if the file is not in the TypeLoader.
            // (Fallthrough)
        }
        let deps = self.all_documents.dependencies.remove(path).unwrap_or_default();
        let mut extra_deps = HashSet::new();
        for dep in &deps {
            extra_deps.extend(self.invalidate_document(dep));
        }
        extra_deps.extend(deps);
        extra_deps
    }

    /// Imports of files that don't have the .slint extension are returned.
    pub async fn load_dependencies_recursively<'a>(
        &'a mut self,
        doc: &'a syntax_nodes::Document,
        diag: &'a mut BuildDiagnostics,
        registry_to_populate: &'a Rc<RefCell<TypeRegister>>,
    ) -> (Vec<ImportedTypes>, Exports) {
        let state = RefCell::new(BorrowedTypeLoader { tl: self, diag });
        Self::load_dependencies_recursively_impl(
            &state,
            doc,
            registry_to_populate,
            &Default::default(),
        )
        .await
    }

    async fn load_dependencies_recursively_impl<'a: 'b, 'b>(
        state: &'a RefCell<BorrowedTypeLoader<'a>>,
        doc: &'b syntax_nodes::Document,
        registry_to_populate: &'b Rc<RefCell<TypeRegister>>,
        import_stack: &'b HashSet<PathBuf>,
    ) -> (Vec<ImportedTypes>, Exports) {
        let mut imports = Vec::new();
        let mut dependencies_futures = Vec::new();
        for mut import in Self::collect_dependencies(state, doc) {
            if matches!(import.import_kind, ImportKind::FileImport) {
                if let Some((path, _)) = state.borrow().tl.resolve_import_path(
                    Some(&import.import_uri_token.clone().into()),
                    &import.file,
                ) {
                    import.file = path.to_string_lossy().into_owned();
                };
                imports.push(import);
                continue;
            }

            dependencies_futures.push(Box::pin(async move {
                #[cfg(feature = "experimental-library-module")]
                let import_file = import.file.clone();
                #[cfg(feature = "experimental-library-module")]
                if let Some(maybe_library_import) = import_file.strip_prefix('@')
                    && let Ok(library_name) = std::env::var(format!(
                        "DEP_{}_SLINT_LIBRARY_NAME",
                        maybe_library_import.to_uppercase()
                    ))
                    && library_name == maybe_library_import
                {
                    let library_slint_source = std::env::var(format!(
                        "DEP_{}_SLINT_LIBRARY_SOURCE",
                        maybe_library_import.to_uppercase()
                    ))
                    .unwrap_or_default();

                    import.file = library_slint_source;

                    if let Ok(library_package) = std::env::var(format!(
                        "DEP_{}_SLINT_LIBRARY_PACKAGE",
                        maybe_library_import.to_uppercase()
                    )) {
                        import.library_info = Some(LibraryInfo {
                            name: library_name,
                            package: library_package,
                            module: std::env::var(format!(
                                "DEP_{}_SLINT_LIBRARY_MODULE",
                                maybe_library_import.to_uppercase()
                            ))
                            .ok(),
                            exports: Vec::new(),
                        });
                    } else {
                        // This should never happen
                        let mut state = state.borrow_mut();
                        state.diag.push_error(
                            format!(
                                "DEP_{}_SLINT_LIBRARY_PACKAGE is missing for external library import",
                                maybe_library_import.to_uppercase()
                            ),
                            &import.import_uri_token.parent(),
                        );
                    }
                }

                let doc_path = Self::ensure_document_loaded(
                    state,
                    import.file.as_str(),
                    Some(import.import_uri_token.clone().into()),
                    import_stack.clone(),
                )
                .await;
                (import, doc_path)
            }));
        }

        let mut reexports = None;
        let mut has_star_reexport = false;
        std::future::poll_fn(|cx| {
            dependencies_futures.retain_mut(|fut| {
                let core::task::Poll::Ready((mut import, doc_path)) = fut.as_mut().poll(cx) else { return true; };
                let doc_path = match doc_path {
                    Ok(doc_path) => doc_path,
                    Err(Some(doc_path)) => {
                        // Even if the import failed (e.g. the file doesn't exist), we need to add it to the document imports so that
                        // the dependency graph is correct and we can retry loading the document if the imported file changes or is created.
                        import.file = doc_path.to_string_lossy().into_owned();
                        imports.push(import);

                        return false;
                    }
                    Err(None) => return false,
                };
                let mut state = state.borrow_mut();
                let state: &mut BorrowedTypeLoader<'a> = &mut state;
                let Some(doc) = state.tl.get_document(&doc_path) else {
                    panic!("Just loaded document not available")
                };
                match &import.import_kind {
                    ImportKind::ImportList(imported_types) => {
                        let mut imported_types = ImportedName::extract_imported_names(imported_types).peekable();
                        if imported_types.peek().is_some() {
                            Self::register_imported_types(doc, &import, imported_types, registry_to_populate, state.diag);

                            #[cfg(feature = "experimental-library-module")]
                            if let Some(library_info) = import.library_info.as_mut() {
                                library_info.exports =
                                    doc.exports.iter().map(|(exported_name, _compo_or_type)| {
                                        exported_name.clone()
                                    }).collect();
                            }
                        } else {
                            state.diag.push_error("Import names are missing. Please specify which types you would like to import".into(), &import.import_uri_token.parent());
                        }
                    }
                    ImportKind::ModuleReexport(export_module_syntax_node) => {
                        let exports = reexports.get_or_insert_with(Exports::default);
                        if let Some(star_reexport) = export_module_syntax_node.ExportModule().and_then(|x| x.child_token(SyntaxKind::Star))
                        {
                            if has_star_reexport {
                                state.diag.push_error("re-exporting modules is only allowed once per file".into(), &star_reexport);
                                return false;
                            }
                            has_star_reexport = true;
                            exports.add_reexports(
                                doc.exports.iter().map(|(exported_name, compo_or_type)| {
                                    let exported_name = ExportedName {
                                        name: exported_name.name.clone(),
                                        name_ident: (**export_module_syntax_node).clone(),
                                    };
                                    (exported_name, compo_or_type.clone())
                                }),
                                state.diag,
                            );
                        } else if export_module_syntax_node.ExportSpecifier().next().is_none() {
                            state.diag.push_error("Import names are missing. Please specify which types you would like to re-export".into(), export_module_syntax_node);
                        } else {
                            let e = export_module_syntax_node
                                .ExportSpecifier()
                                .filter_map(|e| {
                                    let (imported_name, exported_name) = ExportedName::from_export_specifier(&e);
                                    let Some(r) = doc.exports.find(&imported_name) else {
                                        state.diag.push_error(format!("No exported type called '{imported_name}' found in \"{}\"", doc_path.display()), &e);
                                        return None;
                                    };
                                    Some((exported_name, r))
                                })
                                .collect::<Vec<_>>();
                            exports.add_reexports(e, state.diag);
                        }
                    }
                    ImportKind::FileImport => {
                        unreachable!("FileImport should have been handled above")
                    }
                }
                import.file = doc_path.to_string_lossy().into_owned();
                imports.push(import);
                false
            });
            if dependencies_futures.is_empty() {
                core::task::Poll::Ready(())
            } else {
                core::task::Poll::Pending
            }
        }).await;
        (imports, reexports.unwrap_or_default())
    }

    pub async fn import_component(
        &mut self,
        file_to_import: &str,
        type_name: &str,
        diag: &mut BuildDiagnostics,
    ) -> Option<Rc<object_tree::Component>> {
        let state = RefCell::new(BorrowedTypeLoader { tl: self, diag });
        let doc_path =
            match Self::ensure_document_loaded(&state, file_to_import, None, Default::default())
                .await
            {
                Ok(doc_path) => doc_path,
                Err(_) => return None,
            };

        let Some(doc) = self.get_document(&doc_path) else {
            panic!("Just loaded document not available")
        };

        doc.exports.find(type_name).and_then(|compo_or_type| compo_or_type.left())
    }

    /// Append a possibly relative path to a base path. Returns the data if it resolves to a built-in (compiled-in)
    /// file.
    pub fn resolve_import_path(
        &self,
        import_token: Option<&NodeOrToken>,
        maybe_relative_path_or_url: &str,
    ) -> Option<(PathBuf, Option<&'static [u8]>)> {
        if let Some(maybe_library_import) = maybe_relative_path_or_url.strip_prefix('@') {
            self.find_file_in_library_path(maybe_library_import)
        } else {
            let referencing_file_or_url =
                import_token.and_then(|tok| tok.source_file().map(|s| s.path()));
            self.find_file_in_include_path(referencing_file_or_url, maybe_relative_path_or_url)
                .or_else(|| {
                    referencing_file_or_url
                        .and_then(|base_path_or_url| {
                            crate::pathutils::join(
                                &crate::pathutils::dirname(base_path_or_url),
                                &PathBuf::from(maybe_relative_path_or_url),
                            )
                        })
                        .filter(|p| p.exists())
                        .map(|p| (p, None))
                })
        }
    }

    /// Returns whether the file was successfully loaded.
    /// If not, the path that was attempted to be loaded is returned (if any).
    #[allow(clippy::await_holding_refcell_ref)] // false positive: explicit drop() before await
    async fn ensure_document_loaded<'a: 'b, 'b>(
        state: &'a RefCell<BorrowedTypeLoader<'a>>,
        file_to_import: &'b str,
        import_token: Option<NodeOrToken>,
        mut import_stack: HashSet<PathBuf>,
    ) -> Result<PathBuf, Option<PathBuf>> {
        let mut borrowed_state = state.borrow_mut();

        let mut resolved = false;
        let (path_canon, builtin) = match borrowed_state
            .tl
            .resolve_import_path(import_token.as_ref(), file_to_import)
        {
            Some(x) => {
                resolved = true;
                if let Some(file_name) = x.0.file_name().and_then(|f| f.to_str()) {
                    let len = file_to_import.len();
                    if !file_to_import.ends_with(file_name)
                        && len >= file_name.len()
                        && file_name.eq_ignore_ascii_case(
                            file_to_import.get(len - file_name.len()..).unwrap_or(""),
                        )
                        && import_token.as_ref().and_then(|x| x.source_file()).is_some()
                    {
                        borrowed_state.diag.push_warning(
                                format!("Loading \"{file_to_import}\" resolved to a file named \"{file_name}\" with different casing. This behavior is not cross platform. Rename the file, or edit the import to use the same casing"),
                                &import_token,
                            );
                    }
                }
                x
            }
            None => {
                let import_path = crate::pathutils::clean_path(Path::new(file_to_import));
                if import_path.exists() {
                    if import_token.as_ref().and_then(|x| x.source_file()).is_some() {
                        borrowed_state.diag.push_warning(
                        format!(
                            "Loading \"{file_to_import}\" relative to the work directory is deprecated. Files should be imported relative to their import location",
                        ),
                        &import_token,
                    );
                    }
                    (import_path, None)
                } else {
                    // We will load using the `open_import_callback`
                    // Simplify the path to remove the ".."
                    let base_path = import_token
                        .as_ref()
                        .and_then(|tok| tok.source_file().map(|s| s.path()))
                        .map_or(PathBuf::new(), |p| p.into());
                    let path = crate::pathutils::join(
                        &crate::pathutils::dirname(&base_path),
                        Path::new(file_to_import),
                    )
                    .ok_or(None)?;
                    (path, None)
                }
            }
        };

        if !import_stack.insert(path_canon.clone()) {
            borrowed_state.diag.push_error(
                format!("Recursive import of \"{}\"", path_canon.display()),
                &import_token,
            );
            return Err(Some(path_canon));
        }

        drop(borrowed_state);

        let (is_loaded, doc_node) = core::future::poll_fn(|cx| {
            let mut state = state.borrow_mut();
            let all_documents = &mut state.tl.all_documents;
            match all_documents.currently_loading.entry(path_canon.clone()) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let waker = cx.waker();
                    if !e.get().iter().any(|w| w.will_wake(waker)) {
                        e.get_mut().push(cx.waker().clone());
                    }
                    core::task::Poll::Pending
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    match all_documents.docs.get(path_canon.as_path()) {
                        Some((LoadedDocument::Document(_), _)) => {
                            core::task::Poll::Ready((true, None))
                        }
                        Some((LoadedDocument::Invalidated(doc), errors)) => {
                            v.insert(Default::default());
                            core::task::Poll::Ready((false, Some((doc.clone(), errors.clone()))))
                        }
                        None => {
                            v.insert(Default::default());
                            core::task::Poll::Ready((false, None))
                        }
                    }
                }
            }
        })
        .await;
        if is_loaded {
            return Ok(path_canon);
        }

        let doc_node = if let Some((doc_node, errors)) = doc_node {
            for e in errors {
                state.borrow_mut().diag.push_internal_error(e);
            }
            Some(doc_node)
        } else {
            let source_code_result = if let Some(builtin) = builtin {
                Ok(String::from(
                    core::str::from_utf8(builtin)
                        .expect("internal error: embedded file is not UTF-8 source code"),
                ))
            } else {
                let callback = state.borrow().tl.compiler_config.open_import_callback.clone();
                if let Some(callback) = callback {
                    let result = callback(path_canon.to_string_lossy().into()).await;
                    result.unwrap_or_else(|| std::fs::read_to_string(&path_canon))
                } else {
                    std::fs::read_to_string(&path_canon)
                }
            };
            match source_code_result {
                Ok(source) => syntax_nodes::Document::new(if let Some(builtin) = builtin {
                    // Builtin sources (std-widgets, styles, …) are immutable; cache the parsed
                    // green tree across invocations instead of re-parsing every expansion.
                    crate::parser::parse_builtin_cached(
                        builtin,
                        source,
                        Some(&path_canon),
                        state.borrow_mut().diag,
                    )
                } else {
                    crate::parser::parse(source, Some(&path_canon), state.borrow_mut().diag)
                }),
                Err(err)
                    if !resolved
                        && matches!(err.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
                {
                    let import_kind =
                        if file_to_import.starts_with('@') { "library" } else { "include" };
                    state.borrow_mut().diag.push_error(
                        format!(
                            "Cannot find requested import \"{file_to_import}\" in the {import_kind} search path",
                        ),
                        &import_token,
                    );
                    None
                }
                Err(err) => {
                    state.borrow_mut().diag.push_error(
                        format!(
                            "Error reading requested import \"{}\": {}",
                            path_canon.display(),
                            err
                        ),
                        &import_token,
                    );
                    None
                }
            }
        };

        let ok = if let Some(doc_node) = doc_node {
            Self::load_file_impl(state, &path_canon, doc_node, builtin.is_some(), &import_stack)
                .await;
            state.borrow_mut().diag.all_loaded_files.insert(path_canon.clone());
            true
        } else {
            false
        };

        let wakers = state
            .borrow_mut()
            .tl
            .all_documents
            .currently_loading
            .remove(path_canon.as_path())
            .unwrap();
        for x in wakers {
            x.wake();
        }

        if ok { Ok(path_canon) } else { Err(Some(path_canon)) }
    }

    /// Load a file, and its dependency, running only the import passes.
    ///
    /// the path must be the canonical path
    pub async fn load_file(
        &mut self,
        path: &Path,
        source_path: &Path,
        source_code: String,
        is_builtin: bool,
        diag: &mut BuildDiagnostics,
    ) {
        let doc_node: syntax_nodes::Document =
            crate::parser::parse(source_code, Some(source_path), diag).into();
        let state = RefCell::new(BorrowedTypeLoader { tl: self, diag });
        Self::load_file_impl(&state, path, doc_node, is_builtin, &Default::default()).await;
    }

    /// Reload a cached file
    ///
    /// The path must be canonical
    pub async fn reload_cached_file(&mut self, path: &Path, diag: &mut BuildDiagnostics) {
        let Some((LoadedDocument::Invalidated(doc_node), errors)) =
            self.all_documents.docs.get(path)
        else {
            return;
        };
        let doc_node = doc_node.clone();
        for e in errors {
            diag.push_internal_error(e.clone());
        }
        let state = RefCell::new(BorrowedTypeLoader { tl: self, diag });
        Self::load_file_impl(&state, path, doc_node, false, &Default::default()).await;
    }

    /// Load a file, and its dependency, running the full set of passes.
    ///
    /// the path must be the canonical path
    #[allow(clippy::await_holding_refcell_ref)] // requires mutable typeloader+diag through async pass pipeline
    pub async fn load_root_file(
        &mut self,
        path: &Path,
        source_path: &Path,
        source_code: String,
        keep_raw: bool,
        diag: &mut BuildDiagnostics,
    ) -> (PathBuf, Option<TypeLoader>) {
        let path = crate::pathutils::clean_path(path);
        let doc_node: syntax_nodes::Document =
            crate::parser::parse(source_code, Some(source_path), diag).into();
        let parse_errors = diag.iter().cloned().collect();
        let state = RefCell::new(BorrowedTypeLoader { tl: self, diag });
        let (path, mut doc) =
            Self::load_doc_no_pass(&state, &path, doc_node, false, &Default::default()).await;

        let mut state = state.borrow_mut();
        let state = &mut *state;
        let raw_type_loader = if !state.diag.has_errors() {
            crate::passes::run_passes(&mut doc, state.tl, keep_raw, state.diag).await
        } else {
            None
        };
        Self::register_document(state, doc, path.clone(), parse_errors);
        (path, raw_type_loader)
    }

    fn register_document(
        state: &mut BorrowedTypeLoader<'_>,
        doc: Document,
        path: PathBuf,
        parse_errors: Vec<Diagnostic>,
    ) {
        for dep in &doc.imports {
            state
                .tl
                .all_documents
                .dependencies
                .entry(Path::new(&dep.file).into())
                .or_default()
                .insert(path.clone());
        }
        state.tl.all_documents.docs.insert(path, (LoadedDocument::Document(doc), parse_errors));
    }

    async fn load_file_impl<'a>(
        state: &'a RefCell<BorrowedTypeLoader<'a>>,
        path: &Path,
        doc_node: syntax_nodes::Document,
        is_builtin: bool,
        import_stack: &HashSet<PathBuf>,
    ) {
        let parse_errors = state
            .borrow()
            .diag
            .iter()
            .filter(|e| e.source_file().is_some_and(|f| f == path))
            .cloned()
            .collect();
        let (path, doc) =
            Self::load_doc_no_pass(state, path, doc_node, is_builtin, import_stack).await;

        let mut state = state.borrow_mut();
        let state = &mut *state;
        if !state.diag.has_errors() {
            crate::passes::run_import_passes(&doc, state.tl, state.diag);
        }
        Self::register_document(state, doc, path, parse_errors);
    }

    async fn load_doc_no_pass<'a>(
        state: &'a RefCell<BorrowedTypeLoader<'a>>,
        path: &Path,
        dependency_doc: syntax_nodes::Document,
        is_builtin: bool,
        import_stack: &HashSet<PathBuf>,
    ) -> (PathBuf, Document) {
        let dependency_registry =
            Rc::new(RefCell::new(TypeRegister::new(&state.borrow().tl.global_type_registry)));
        dependency_registry.borrow_mut().expose_internal_types =
            is_builtin || state.borrow().tl.compiler_config.enable_experimental;
        let (imports, reexports) = Self::load_dependencies_recursively_impl(
            state,
            &dependency_doc,
            &dependency_registry,
            import_stack,
        )
        .await;

        if state.borrow().diag.has_errors() {
            // If there was error (esp parse error) we don't want to report further error in this document.
            // because they might be nonsense (TODO: we should check that the parse error were really in this document).
            // But we still want to create a document to give better error messages in the root document.
            let mut ignore_diag = BuildDiagnostics::default();
            ignore_diag.push_error_with_span(
                "Dummy error because some of the code asserts there was an error".into(),
                Default::default(),
            );
            let doc = crate::object_tree::Document::from_node(
                dependency_doc,
                imports,
                reexports,
                &mut ignore_diag,
                &dependency_registry,
            );
            return (path.to_owned(), doc);
        }
        let mut state = state.borrow_mut();
        let state = &mut *state;
        let doc = crate::object_tree::Document::from_node(
            dependency_doc,
            imports,
            reexports,
            state.diag,
            &dependency_registry,
        );
        (path.to_owned(), doc)
    }

    fn register_imported_types(
        doc: &Document,
        import: &ImportedTypes,
        imported_types: impl Iterator<Item = ImportedName>,
        registry_to_populate: &Rc<RefCell<TypeRegister>>,
        build_diagnostics: &mut BuildDiagnostics,
    ) {
        for import_name in imported_types {
            let imported_type = doc.exports.find(&import_name.external_name);

            let imported_type = match imported_type {
                Some(ty) => ty,
                None => {
                    build_diagnostics.push_error(
                        format!(
                            "No exported type called '{}' found in \"{}\"",
                            import_name.external_name, import.file
                        ),
                        &import.import_uri_token,
                    );
                    continue;
                }
            };

            match imported_type {
                itertools::Either::Left(c) => {
                    registry_to_populate.borrow_mut().add_with_name(import_name.internal_name, c)
                }
                itertools::Either::Right(ty) => registry_to_populate
                    .borrow_mut()
                    .insert_type_with_name(ty, import_name.internal_name),
            };
        }
    }

    /// Lookup a library and filename and try to find the absolute filename based on the library path
    fn find_file_in_library_path(
        &self,
        maybe_library_import: &str,
    ) -> Option<(PathBuf, Option<&'static [u8]>)> {
        let (library, file) = maybe_library_import
            .splitn(2, '/')
            .collect_tuple()
            .map(|(library, path)| (library, Some(path)))
            .unwrap_or((maybe_library_import, None));
        self.compiler_config.library_paths.get(library).and_then(|library_path| {
            let path = match file {
                // "@library/file.slint" -> "/path/to/library/" + "file.slint"
                Some(file) => library_path.join(file),
                // "@library" -> "/path/to/library/lib.slint"
                None => library_path.clone(),
            };
            crate::fileaccess::load_file(path.as_path())
                .map(|virtual_file| (virtual_file.canon_path, virtual_file.builtin_contents))
                .or(Some((path, None)))
        })
    }

    /// Lookup a filename and try to find the absolute filename based on the include path or
    /// the current file directory
    pub fn find_file_in_include_path(
        &self,
        referencing_file: Option<&Path>,
        file_to_import: &str,
    ) -> Option<(PathBuf, Option<&'static [u8]>)> {
        // The directory of the current file is the first in the list of include directories.
        referencing_file
            .and_then(|x| x.parent().map(|x| x.to_path_buf()))
            .into_iter()
            .chain(referencing_file.and_then(maybe_base_directory))
            .chain(self.compiler_config.include_paths.iter().map(PathBuf::as_path).map(
                |include_path| {
                    let base = referencing_file.map(Path::to_path_buf).unwrap_or_default();
                    crate::pathutils::join(&crate::pathutils::dirname(&base), include_path)
                        .unwrap_or_else(|| include_path.to_path_buf())
                },
            ))
            .chain(
                (file_to_import == "std-widgets.slint"
                    || (file_to_import == "style-base.slint" && referencing_file.is_none())
                    || (file_to_import == "std-widgets-impl.slint" && referencing_file.is_none())
                    || referencing_file.is_some_and(|x| x.starts_with("builtin:/")))
                .then(|| format!("builtin:/{}", self.resolved_style).into()),
            )
            .find_map(|include_dir| {
                let candidate = crate::pathutils::join(&include_dir, Path::new(file_to_import))?;
                crate::fileaccess::load_file(&candidate)
                    .map(|virtual_file| (virtual_file.canon_path, virtual_file.builtin_contents))
            })
    }

    fn collect_dependencies<'a: 'b, 'b>(
        state: &'a RefCell<BorrowedTypeLoader<'a>>,
        doc: &'b syntax_nodes::Document,
    ) -> impl Iterator<Item = ImportedTypes> + 'a {
        doc.ImportSpecifier()
            .map(|import| {
                let maybe_import_uri = import.child_token(SyntaxKind::StringLiteral);

                let kind = import
                    .ImportIdentifierList()
                    .map(ImportKind::ImportList)
                    .unwrap_or(ImportKind::FileImport);
                (maybe_import_uri, kind)
            })
            .chain(
                // process `export ... from "foo"`
                doc.ExportsList().filter_map(|exports| {
                    exports.ExportModule().map(|reexport| {
                        let maybe_import_uri = reexport.child_token(SyntaxKind::StringLiteral);
                        (maybe_import_uri, ImportKind::ModuleReexport(exports))
                    })
                }),
            )
            .filter_map(|(maybe_import_uri, type_specifier)| {
                let import_uri = match maybe_import_uri {
                    Some(import_uri) => import_uri,
                    None => {
                        debug_assert!(state.borrow().diag.has_errors());
                        return None;
                    }
                };
                let path_to_import = import_uri.text().to_string();
                let path_to_import = path_to_import.trim_matches('\"').to_string();

                if path_to_import.is_empty() {
                    state
                        .borrow_mut()
                        .diag
                        .push_error("Unexpected empty import url".to_owned(), &import_uri);
                    return None;
                }

                Some(ImportedTypes {
                    import_uri_token: import_uri,
                    import_kind: type_specifier,
                    file: path_to_import,
                    library_info: None,
                })
            })
    }

    /// Return a document if it was already loaded
    pub fn get_document<'b>(&'b self, path: &Path) -> Option<&'b object_tree::Document> {
        let path = crate::pathutils::clean_path(path);
        if let Some((LoadedDocument::Document(d), _)) = self.all_documents.docs.get(&path) {
            Some(d)
        } else {
            None
        }
    }

    /// Return an iterator over all the loaded file path
    pub fn all_files(&self) -> impl Iterator<Item = &PathBuf> {
        self.all_documents.docs.keys()
    }

    /// Returns all file paths whose on-disk changes can affect the current document graph.
    ///
    /// This includes loaded documents and unresolved import targets that are kept in the
    /// dependency graph so newly created files can invalidate their dependents.
    pub fn all_files_to_watch(&self) -> HashSet<PathBuf> {
        // Note: This only works if the full set of passes have run (e.g. in load_root_file, but not
        // in load_file).
        //
        // TODO: the LSP will only run the import passes, which do not yet
        // detect embedded file resources, so we won't know about them until we
        // run the full pass pipeline (e.g. in the editor binary).
        fn resource_paths(document: &LoadedDocument) -> Vec<PathBuf> {
            match document {
                LoadedDocument::Document(document) => document
                    .embedded_file_resources
                    .borrow()
                    .iter()
                    .flat_map(|resource| resource.path.as_ref().map(|path| PathBuf::from(&**path)))
                    .collect(),
                LoadedDocument::Invalidated(_document) => vec![],
            }
        }

        self.all_documents
            .docs
            .iter()
            .flat_map(|(path, (document, _diagnostics))| {
                std::iter::once(path.clone()).chain(resource_paths(document))
            })
            .chain(self.all_documents.dependencies.keys().cloned())
            .collect()
    }

    /// Returns an iterator over all the loaded documents
    pub fn all_documents(&self) -> impl Iterator<Item = &object_tree::Document> + '_ {
        self.all_documents.docs.values().filter_map(|(d, _)| match d {
            LoadedDocument::Document(d) => Some(d),
            LoadedDocument::Invalidated(_) => None,
        })
    }

    /// Returns an iterator over all the loaded documents
    pub fn all_file_documents(
        &self,
    ) -> impl Iterator<Item = (&PathBuf, &syntax_nodes::Document)> + '_ {
        self.all_documents.docs.iter().filter_map(|(p, (d, _))| {
            Some((
                p,
                match d {
                    LoadedDocument::Document(d) => d.node.as_ref()?,
                    LoadedDocument::Invalidated(d) => d,
                },
            ))
        })
    }
}

fn get_native_style(all_loaded_files: &mut std::collections::BTreeSet<PathBuf>) -> String {
    // Try to get the value written by the i-slint-backend-selector's build script

    // It is in the target/xxx/build directory
    let target_path = std::env::var_os("OUT_DIR")
        .and_then(|path| {
            // Same logic as in i-slint-backend-selector's build script to get the path
            crate::pathutils::join(Path::new(&path), Path::new("../../SLINT_DEFAULT_STYLE.txt"))
        })
        .or_else(|| {
            // When we are called from a slint!, OUT_DIR is only defined when the crate having the macro has a build.rs script.
            // As a fallback, try to parse the rustc arguments
            // https://stackoverflow.com/questions/60264534/getting-the-target-folder-from-inside-a-rust-proc-macro
            let mut args = std::env::args();
            let mut out_dir = None;
            while let Some(arg) = args.next() {
                if arg == "--out-dir" {
                    out_dir = args.next();
                    break;
                }
            }
            out_dir.and_then(|od| {
                crate::pathutils::join(
                    Path::new(&od),
                    Path::new("../build/SLINT_DEFAULT_STYLE.txt"),
                )
            })
        });

    if let Some(style) = target_path.and_then(|target_path| {
        std::fs::read_to_string(&target_path)
            .map(|style| {
                all_loaded_files.insert(target_path);
                style.trim().into()
            })
            .ok()
    }) {
        return style;
    }
    i_slint_common::get_native_style(false, &std::env::var("TARGET").unwrap_or_default()).into()
}

/// For a .rs file, return the manifest directory
///
/// This is for compatibility with `slint!` macro as before rust 1.88,
/// it was not possible for the macro to know the current path and
/// the Cargo.toml file was used instead
fn maybe_base_directory(referencing_file: &Path) -> Option<PathBuf> {
    if referencing_file.extension().is_some_and(|e| e == "rs") {
        // For .rs file, this is a rust macro, and rust macro locates the file relative to the CARGO_MANIFEST_DIR which is the directory that has a Cargo.toml file.
        let mut candidate = referencing_file;
        loop {
            candidate =
                if let Some(c) = candidate.parent() { c } else { break referencing_file.parent() };

            if candidate.join("Cargo.toml").exists() {
                break Some(candidate);
            }
        }
        .map(|x| x.to_path_buf())
    } else {
        None
    }
}

/// Measures whether reusing a pre-compiled std-widgets `TypeLoader` via `snapshot()` (a deep
/// clone + re-link) is cheaper than recompiling std-widgets from scratch on every expansion.
///
/// This gates the "cache the compiled builtin documents" lever: if `snapshot()` is much
/// cheaper than the std-widgets compile portion, keeping a template loader alive and snapshotting
/// it per expansion is worth pursuing; if not, only a deeper immutable-definitions refactor
/// would help.
///
/// Run:
/// `RUN_SLOW_BENCHES=1 cargo test --release -p i-slint-compiler typeloader::bench_snapshot_vs_recompile -- --nocapture --exact`
#[test]
fn bench_snapshot_vs_recompile() {
    if std::env::var("RUN_SLOW_BENCHES").is_err() {
        return;
    }
    let iters: u32 =
        std::env::var("SLINT_BENCH_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(40);

    let import = "import { Button, Slider, ComboBox, CheckBox, LineEdit, ProgressIndicator, \
         VerticalBox, HorizontalBox, GroupBox, TabWidget } from \"std-widgets.slint\";";
    let src_import = format!(
        "{import}\nexport component Bench {{ VerticalBox {{ Button {{ text: \"x\"; }} }} }}"
    );
    let src_trivial = "export component Bench { Rectangle { width: 10px; } }".to_string();

    let fresh_setup = |src: &str| {
        let mut diag = BuildDiagnostics::default();
        let node: syntax_nodes::Document =
            crate::parser::parse(src.to_string(), Some(Path::new("bench.slint")), &mut diag).into();
        let mut cc = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
        cc.style = Some("fluent".into());
        // Disable the prototype semantic cache for the recompile leg while still resolving the
        // embedded builtin files after this nonexistent include path misses.
        cc.include_paths = vec![PathBuf::from("/__slint_bench_no_such_include_path__")];
        let mut loader = TypeLoader::new(cc, &mut diag);
        let reg = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));
        spin_on::spin_on(loader.load_dependencies_recursively(&node, &mut diag, &reg));
        assert!(!diag.has_errors(), "bench source failed to load");
        loader
    };

    let template = fresh_setup(&src_import);
    assert!(snapshot(&template).is_some(), "snapshot must succeed on a raw loader");

    let mean_ms = |f: &dyn Fn()| {
        f();
        let t = std::time::Instant::now();
        for _ in 0..iters {
            f();
        }
        t.elapsed().as_secs_f64() * 1000.0 / iters as f64
    };

    let fresh_import = mean_ms(&|| {
        std::hint::black_box(fresh_setup(&src_import));
    });
    let fresh_trivial = mean_ms(&|| {
        std::hint::black_box(fresh_setup(&src_trivial));
    });
    let snapshot_ms = mean_ms(&|| {
        std::hint::black_box(snapshot(&template).expect("snapshot failed"));
    });
    let freeze_ms = mean_ms(&|| {
        std::hint::black_box(template.freeze_builtin_semantic_metadata());
    });

    let mut cached_cc = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    cached_cc.style = Some("fluent".into());
    let frozen_key =
        crate::frozen_builtins::FrozenBuiltinCacheKey::from_config(&cached_cc, "fluent").unwrap();
    crate::frozen_builtins::store(frozen_key.clone(), template.freeze_builtin_semantic_metadata());
    let frozen_lookup_ms = mean_ms(&|| {
        std::hint::black_box(
            crate::frozen_builtins::get(&frozen_key).expect("frozen metadata cache miss"),
        );
    });
    let frozen = crate::frozen_builtins::get(&frozen_key).expect("frozen metadata cache miss");
    let frozen_encode_ms = mean_ms(&|| {
        std::hint::black_box(serde_json::to_vec(&frozen).expect("frozen artifact encode failed"));
    });
    let frozen_encoded = serde_json::to_vec(&frozen).expect("frozen artifact encode failed");
    let frozen_decode_ms = mean_ms(&|| {
        std::hint::black_box(
            serde_json::from_slice::<crate::frozen_builtins::FrozenBuiltinLibrary>(&frozen_encoded)
                .expect("frozen artifact decode failed"),
        );
    });
    let frozen_binary_encode_ms = mean_ms(&|| {
        std::hint::black_box(
            postcard::to_allocvec(&frozen).expect("binary artifact encode failed"),
        );
    });
    let frozen_binary_encoded =
        postcard::to_allocvec(&frozen).expect("binary artifact encode failed");
    let generated_static_artifact: &'static [u8] =
        Box::leak(frozen_binary_encoded.clone().into_boxed_slice());
    crate::frozen_builtins::store_generated_artifact(frozen_key.clone(), generated_static_artifact);
    let generated_artifact_lookup_ms = mean_ms(&|| {
        std::hint::black_box(
            crate::frozen_builtins::generated_artifact(&frozen_key)
                .expect("generated artifact lookup failed"),
        );
    });
    let frozen_binary_artifact_path = std::env::temp_dir()
        .join(format!("slint-frozen-builtin-artifact-{}-{iters}.postcard", std::process::id()));
    std::fs::write(&frozen_binary_artifact_path, &frozen_binary_encoded)
        .expect("failed to write binary artifact");
    let frozen_binary_file_read_ms = mean_ms(&|| {
        std::hint::black_box(
            std::fs::read(&frozen_binary_artifact_path).expect("binary artifact read failed"),
        );
    });
    let frozen_binary_decode_ms = mean_ms(&|| {
        std::hint::black_box(
            postcard::from_bytes::<crate::frozen_builtins::FrozenBuiltinLibrary>(
                &frozen_binary_encoded,
            )
            .expect("binary artifact decode failed"),
        );
    });
    let parent_registry = TypeRegister::builtin();
    let skeleton_rehydrate_ms = mean_ms(&|| {
        std::hint::black_box(frozen.rehydrate_component_skeletons(&parent_registry));
    });
    let snapshot_parent_registry = || {
        let parent_loader = TypeLoader {
            global_type_registry: parent_registry.clone(),
            compiler_config: cached_cc.clone(),
            resolved_style: "fluent".into(),
            all_documents: Default::default(),
        };
        snapshot(&parent_loader).expect("parent registry snapshot failed").global_type_registry
    };
    let parent_registry_fresh_ms = mean_ms(&|| {
        std::hint::black_box(TypeRegister::builtin());
    });
    let parent_registry_snapshot_ms = mean_ms(&|| {
        std::hint::black_box(snapshot_parent_registry());
    });
    let parent_registry_freeze_ms = mean_ms(&|| {
        std::hint::black_box(parent_registry.borrow().freeze_builtin_registry_metadata());
    });
    let frozen_parent_registry = parent_registry.borrow().freeze_builtin_registry_metadata();
    let cached_frozen_parent_registry = frozen.parent_registry.clone();
    let parent_registry_shell_rehydrate_ms = mean_ms(&|| {
        std::hint::black_box(TypeRegister::rehydrate_builtin_registry_shell(
            &frozen_parent_registry,
        ));
    });
    let skeleton_rehydrate_with_shell_parent_ms = mean_ms(&|| {
        let parent_registry =
            TypeRegister::rehydrate_builtin_registry_shell(&frozen_parent_registry);
        std::hint::black_box(frozen.rehydrate_component_skeletons(&parent_registry));
    });
    let frozen_artifact_rehydrate_ms = mean_ms(&|| {
        let frozen = crate::frozen_builtins::get(&frozen_key).expect("frozen metadata cache miss");
        let parent_registry = frozen.rehydrate_parent_registry();
        std::hint::black_box(frozen.rehydrate_component_skeletons(&parent_registry));
    });
    let frozen_loader_rehydrate_ms = mean_ms(&|| {
        let frozen = crate::frozen_builtins::get(&frozen_key).expect("frozen metadata cache miss");
        std::hint::black_box(
            TypeLoader::rehydrate_frozen_builtin_artifact(
                cached_cc.clone(),
                "fluent".into(),
                &frozen,
            )
            .expect("frozen artifact restore failed"),
        );
    });
    let frozen_decode_loader_rehydrate_ms = mean_ms(&|| {
        let frozen =
            serde_json::from_slice::<crate::frozen_builtins::FrozenBuiltinLibrary>(&frozen_encoded)
                .expect("frozen artifact decode failed");
        std::hint::black_box(
            TypeLoader::rehydrate_frozen_builtin_artifact(
                cached_cc.clone(),
                "fluent".into(),
                &frozen,
            )
            .expect("decoded frozen artifact restore failed"),
        );
    });
    let frozen_binary_decode_loader_rehydrate_ms = mean_ms(&|| {
        let frozen = postcard::from_bytes::<crate::frozen_builtins::FrozenBuiltinLibrary>(
            &frozen_binary_encoded,
        )
        .expect("binary artifact decode failed");
        std::hint::black_box(
            TypeLoader::rehydrate_frozen_builtin_artifact(
                cached_cc.clone(),
                "fluent".into(),
                &frozen,
            )
            .expect("binary frozen artifact restore failed"),
        );
    });
    let frozen_binary_file_loader_rehydrate_ms = mean_ms(&|| {
        let encoded =
            std::fs::read(&frozen_binary_artifact_path).expect("binary artifact read failed");
        let frozen = postcard::from_bytes::<crate::frozen_builtins::FrozenBuiltinLibrary>(&encoded)
            .expect("binary artifact decode failed");
        std::hint::black_box(
            TypeLoader::rehydrate_frozen_builtin_artifact(
                cached_cc.clone(),
                "fluent".into(),
                &frozen,
            )
            .expect("file-backed binary frozen artifact restore failed"),
        );
    });
    let generated_loader_rehydrate_ms = mean_ms(&|| {
        std::hint::black_box(
            TypeLoader::restore_generated_builtin_artifact_for(&cached_cc, "fluent")
                .expect("generated artifact restore failed"),
        );
    });
    let skeleton_rehydrate_with_cached_shell_parent_ms = mean_ms(&|| {
        let parent_registry =
            TypeRegister::rehydrate_builtin_registry_shell(&cached_frozen_parent_registry);
        std::hint::black_box(frozen.rehydrate_component_skeletons(&parent_registry));
    });
    let skeleton_rehydrate_with_snapshot_parent_ms = mean_ms(&|| {
        let parent_registry = snapshot_parent_registry();
        std::hint::black_box(frozen.rehydrate_component_skeletons(&parent_registry));
    });
    let skeleton_rehydrate_with_parent_ms = mean_ms(&|| {
        let parent_registry = TypeRegister::builtin();
        std::hint::black_box(frozen.rehydrate_component_skeletons(&parent_registry));
    });

    eprintln!("\nsnapshot vs recompile ({iters} iters each):");
    eprintln!("  new loader + load std-widgets : {fresh_import:7.2} ms  (what snapshot replaces)");
    eprintln!("  new loader, no import         : {fresh_trivial:7.2} ms  (global register build)");
    eprintln!("  => std-widgets load portion   : {:7.2} ms", fresh_import - fresh_trivial);
    eprintln!("  snapshot(template)            : {snapshot_ms:7.2} ms");
    eprintln!("  => saving vs new+load         : {:7.2} ms", fresh_import - snapshot_ms);
    eprintln!("  freeze metadata               : {freeze_ms:7.2} ms");
    eprintln!("  frozen cache lookup+clone     : {frozen_lookup_ms:7.2} ms");
    eprintln!("  frozen artifact encode        : {frozen_encode_ms:7.2} ms");
    eprintln!(
        "  frozen artifact decode        : {frozen_decode_ms:7.2} ms  ({} bytes JSON)",
        frozen_encoded.len()
    );
    eprintln!("  binary artifact encode        : {frozen_binary_encode_ms:7.2} ms");
    eprintln!("  generated artifact lookup     : {generated_artifact_lookup_ms:7.2} ms");
    eprintln!("  binary artifact file read     : {frozen_binary_file_read_ms:7.2} ms");
    eprintln!(
        "  binary artifact decode        : {frozen_binary_decode_ms:7.2} ms  ({} bytes postcard)",
        frozen_binary_encoded.len()
    );
    eprintln!("  skeleton rehydrate            : {skeleton_rehydrate_ms:7.2} ms");
    eprintln!("  parent registry fresh         : {parent_registry_fresh_ms:7.2} ms");
    eprintln!("  parent registry snapshot      : {parent_registry_snapshot_ms:7.2} ms");
    eprintln!("  parent registry freeze        : {parent_registry_freeze_ms:7.2} ms");
    eprintln!("  parent registry shell rehyd   : {parent_registry_shell_rehydrate_ms:7.2} ms");
    eprintln!("  skeleton + shell parent       : {skeleton_rehydrate_with_shell_parent_ms:7.2} ms");
    eprintln!(
        "  skeleton + cached shell parent: {skeleton_rehydrate_with_cached_shell_parent_ms:7.2} ms"
    );
    eprintln!("  frozen artifact rehydrate     : {frozen_artifact_rehydrate_ms:7.2} ms");
    eprintln!("  frozen loader rehydrate       : {frozen_loader_rehydrate_ms:7.2} ms");
    eprintln!("  decode + loader rehydrate     : {frozen_decode_loader_rehydrate_ms:7.2} ms");
    eprintln!(
        "  binary decode + loader rehyd  : {frozen_binary_decode_loader_rehydrate_ms:7.2} ms"
    );
    eprintln!("  file + binary decode + loader : {frozen_binary_file_loader_rehydrate_ms:7.2} ms");
    eprintln!("  generated artifact restore    : {generated_loader_rehydrate_ms:7.2} ms");
    eprintln!(
        "  skeleton + snapshot parent    : {skeleton_rehydrate_with_snapshot_parent_ms:7.2} ms"
    );
    eprintln!("  skeleton rehydrate + builtins : {skeleton_rehydrate_with_parent_ms:7.2} ms");
    std::fs::remove_file(&frozen_binary_artifact_path).ok();
}

#[test]
fn test_dependency_loading() {
    let test_source_path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "typeloader"].iter().collect();

    let mut incdir = test_source_path.clone();
    incdir.push("incpath");

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.include_paths = vec![incdir];
    compiler_config.library_paths =
        HashMap::from([("library".into(), test_source_path.join("library").join("lib.slint"))]);
    compiler_config.style = Some("fluent".into());

    let mut main_test_path = test_source_path.clone();
    main_test_path.push("dependency_test_main.slint");

    let mut test_diags = crate::diagnostics::BuildDiagnostics::default();
    let doc_node = crate::parser::parse_file(&main_test_path, &mut test_diags).unwrap();

    let doc_node: syntax_nodes::Document = doc_node.into();

    let mut build_diagnostics = BuildDiagnostics::default();

    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    let registry = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));

    let (foreign_imports, _) = spin_on::spin_on(loader.load_dependencies_recursively(
        &doc_node,
        &mut build_diagnostics,
        &registry,
    ));

    assert!(!test_diags.has_errors());
    assert!(!build_diagnostics.has_errors());
    assert_eq!(foreign_imports.len(), 3);
    assert!(foreign_imports.iter().all(|x| matches!(x.import_kind, ImportKind::ImportList(..))));

    let imported_files: Vec<_> = [
        "incpath/local_helper_type.slint",
        "incpath/dependency_from_incpath.slint",
        "dependency_local.slint",
        "library/lib.slint",
        "library/dependency_from_library.slint",
    ]
    .into_iter()
    .map(|path| test_source_path.join(path))
    .collect();
    for file in &imported_files {
        assert!(loader.get_document(file).is_some());
    }

    // Test Typeloader invalidation/dropping
    // Dropping/invalidating all leaf nodes should invalidate everything.
    let to_drop = test_source_path.join("incpath/local_helper_type.slint");
    loader.drop_document(&to_drop).unwrap();
    let to_invalidate = test_source_path.join("library/dependency_from_library.slint");
    loader.invalidate_document(&to_invalidate);

    // Check that the dropped file has indeed been fully dropped.
    assert!(!loader.all_files().contains(&to_drop));
    // But that the invalidated file is still there (even if get_document won't return it anymore)
    assert!(loader.all_files().contains(&to_invalidate));

    for file in imported_files {
        assert!(loader.get_document(&file).is_none(), "{} is still loaded", file.display());
    }
}

#[test]
fn test_dependency_loading_from_rust() {
    let test_source_path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "typeloader"].iter().collect();

    let mut incdir = test_source_path.clone();
    incdir.push("incpath");

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.include_paths = vec![incdir];
    compiler_config.library_paths =
        HashMap::from([("library".into(), test_source_path.join("library").join("lib.slint"))]);
    compiler_config.style = Some("fluent".into());

    let mut main_test_path = test_source_path;
    main_test_path.push("some_rust_file.rs");

    let mut test_diags = crate::diagnostics::BuildDiagnostics::default();
    let doc_node = crate::parser::parse_file(main_test_path, &mut test_diags).unwrap();

    let doc_node: syntax_nodes::Document = doc_node.into();

    let mut build_diagnostics = BuildDiagnostics::default();

    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    let registry = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));

    let (foreign_imports, _) = spin_on::spin_on(loader.load_dependencies_recursively(
        &doc_node,
        &mut build_diagnostics,
        &registry,
    ));

    assert!(!test_diags.has_errors());
    assert!(test_diags.is_empty()); // also no warnings
    assert!(!build_diagnostics.has_errors());
    assert!(build_diagnostics.is_empty()); // also no warnings
    assert_eq!(foreign_imports.len(), 3);
    assert!(foreign_imports.iter().all(|x| matches!(x.import_kind, ImportKind::ImportList(..))));
}

#[test]
fn test_builtin_semantic_cache_rehydrates_documents() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, first_loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));

    assert!(!compile_diags.has_errors());
    assert!(first_loader.get_document(Path::new("builtin:/fluent/std-widgets.slint")).is_some());

    let mut loader_diags = BuildDiagnostics::default();
    let cached_loader = TypeLoader::new(compiler_config, &mut loader_diags);

    assert!(!loader_diags.has_errors());
    assert!(cached_loader.get_document(Path::new("builtin:/fluent/std-widgets.slint")).is_some());
}

#[test]
fn test_builtin_semantic_cache_rehydrates_style_base_documents() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"export component Test { Rectangle {} }"#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, first_loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));

    assert!(!compile_diags.has_errors());
    assert!(first_loader.get_document(Path::new("builtin:/fluent/style-base.slint")).is_some());

    let mut loader_diags = BuildDiagnostics::default();
    let cached_loader = TypeLoader::new(compiler_config, &mut loader_diags);

    assert!(!loader_diags.has_errors());
    assert!(cached_loader.get_document(Path::new("builtin:/fluent/style-base.slint")).is_some());
}

#[test]
fn test_frozen_builtin_metadata_is_process_global_safe() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<crate::frozen_builtins::FrozenBuiltinLibrary>();

    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, loader) =
        spin_on::spin_on(crate::compile_syntax_node(doc_node, parse_diags, compiler_config));
    assert!(!compile_diags.has_errors());

    let frozen = loader.freeze_builtin_semantic_metadata();
    assert_eq!(frozen.schema_version, crate::frozen_builtins::FROZEN_BUILTIN_SCHEMA_VERSION);
    let std_widgets = frozen
        .documents
        .iter()
        .find(|doc| doc.path == "builtin:/fluent/std-widgets.slint")
        .expect("std-widgets metadata should be frozen");

    assert!(std_widgets.exports.iter().any(|export| {
        export.name == "Button"
            && export.kind == crate::frozen_builtins::FrozenBuiltinExportKind::Component
    }));
    assert!(frozen.documents.iter().flat_map(|doc| &doc.components).any(|component| {
        !component.id.is_empty()
            && !component.root_element.base_type.is_empty()
            && !component.root_element.children.is_empty()
    }));
    assert!(frozen.parent_registry.elements.iter().any(|element| element.name == "Rectangle"));
    let parent_registry = frozen.rehydrate_parent_registry();
    assert!(matches!(
        parent_registry.borrow().lookup_element("Rectangle"),
        Ok(langtype::ElementType::Builtin(_))
    ));
}

#[test]
fn test_frozen_builtin_metadata_is_stored_in_process_global_cache() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, _loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));
    assert!(!compile_diags.has_errors());

    let frozen = TypeLoader::cached_frozen_builtin_metadata(&compiler_config, "fluent")
        .expect("frozen builtin metadata should be cached");
    let std_widgets = frozen
        .documents
        .iter()
        .find(|doc| doc.path == "builtin:/fluent/std-widgets.slint")
        .expect("std-widgets metadata should be cached");

    assert!(std_widgets.exports.iter().any(|export| {
        export.name == "Button"
            && export.kind == crate::frozen_builtins::FrozenBuiltinExportKind::Component
    }));
    assert!(frozen.parent_registry.elements.iter().any(|element| element.name == "Platform"));
    let parent_registry = frozen.rehydrate_parent_registry();
    assert!(matches!(
        parent_registry.borrow().lookup_element("Platform"),
        Ok(langtype::ElementType::Component(component)) if component.is_global()
    ));
}

#[test]
fn test_frozen_builtin_artifact_rehydrates_loader_documents() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, _loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));
    assert!(!compile_diags.has_errors());

    let frozen = TypeLoader::cached_frozen_builtin_metadata(&compiler_config, "fluent")
        .expect("frozen builtin metadata should be cached");
    let mut loader =
        TypeLoader::rehydrate_frozen_builtin_artifact(compiler_config, "fluent".into(), &frozen)
            .expect("frozen artifact should rehydrate");

    assert!(loader.get_document(Path::new("builtin:/fluent/std-widgets.slint")).is_some());
    assert!(loader.get_document(Path::new("builtin:/fluent/style-base.slint")).is_some());

    let mut import_diags = BuildDiagnostics::default();
    let button =
        spin_on::spin_on(loader.import_component("std-widgets.slint", "Button", &mut import_diags))
            .expect("Button should import from the rehydrated frozen artifact");
    assert!(!import_diags.has_errors());
    assert_eq!(button.id.as_str(), "Button");
}

#[test]
fn test_frozen_builtin_artifact_serialization_round_trip_rehydrates_loader() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));
    assert!(!compile_diags.has_errors());

    let frozen = loader.freeze_builtin_semantic_metadata();
    let encoded = serde_json::to_vec(&frozen).expect("frozen artifact should serialize");
    assert!(!encoded.is_empty());
    let decoded: crate::frozen_builtins::FrozenBuiltinLibrary =
        serde_json::from_slice(&encoded).expect("frozen artifact should deserialize");

    let mut loader =
        TypeLoader::rehydrate_frozen_builtin_artifact(compiler_config, "fluent".into(), &decoded)
            .expect("decoded frozen artifact should rehydrate");

    let mut import_diags = BuildDiagnostics::default();
    let button =
        spin_on::spin_on(loader.import_component("std-widgets.slint", "Button", &mut import_diags))
            .expect("Button should import from a decoded frozen artifact");
    assert!(!import_diags.has_errors());
    assert_eq!(button.id.as_str(), "Button");

    let parent_registry = decoded.rehydrate_parent_registry();
    assert!(matches!(
        parent_registry.borrow().lookup_element("Platform"),
        Ok(langtype::ElementType::Component(component)) if component.is_global()
    ));
}

#[test]
fn test_frozen_builtin_artifact_rejects_unsupported_schema_version() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));
    assert!(!compile_diags.has_errors());

    let mut frozen = loader.freeze_builtin_semantic_metadata();
    frozen.schema_version = crate::frozen_builtins::FROZEN_BUILTIN_SCHEMA_VERSION + 1;

    assert!(
        TypeLoader::rehydrate_frozen_builtin_artifact(compiler_config, "fluent".into(), &frozen,)
            .is_none()
    );
}

#[test]
fn test_frozen_builtin_binary_artifact_static_slice_rehydrates_loader() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));
    assert!(!compile_diags.has_errors());

    let frozen = loader.freeze_builtin_semantic_metadata();
    let encoded = postcard::to_allocvec(&frozen).expect("frozen artifact should serialize");
    assert!(!encoded.is_empty());

    // Stand-in for the final generated `static FROZEN_BUILTINS: &[u8] = include_bytes!(...)`.
    let static_artifact: &'static [u8] = Box::leak(encoded.into_boxed_slice());
    let decoded: crate::frozen_builtins::FrozenBuiltinLibrary =
        postcard::from_bytes(static_artifact).expect("frozen artifact should deserialize");

    let mut loader =
        TypeLoader::rehydrate_frozen_builtin_artifact(compiler_config, "fluent".into(), &decoded)
            .expect("binary frozen artifact should rehydrate");
    let mut import_diags = BuildDiagnostics::default();
    let button =
        spin_on::spin_on(loader.import_component("std-widgets.slint", "Button", &mut import_diags))
            .expect("Button should import from a decoded binary frozen artifact");
    assert!(!import_diags.has_errors());
    assert_eq!(button.id.as_str(), "Button");
}

#[test]
fn test_frozen_builtin_binary_artifact_file_rehydrates_loader() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));
    assert!(!compile_diags.has_errors());

    let frozen = loader.freeze_builtin_semantic_metadata();
    let encoded = postcard::to_allocvec(&frozen).expect("frozen artifact should serialize");
    let path = std::env::temp_dir()
        .join(format!("slint-frozen-builtin-artifact-test-{}.postcard", std::process::id()));
    std::fs::write(&path, &encoded).expect("frozen artifact file should be written");

    let encoded = std::fs::read(&path).expect("frozen artifact file should be read");
    std::fs::remove_file(&path).ok();
    let decoded: crate::frozen_builtins::FrozenBuiltinLibrary =
        postcard::from_bytes(&encoded).expect("frozen artifact should deserialize from file");

    let mut loader =
        TypeLoader::rehydrate_frozen_builtin_artifact(compiler_config, "fluent".into(), &decoded)
            .expect("file-backed binary frozen artifact should rehydrate");
    let mut import_diags = BuildDiagnostics::default();
    let button =
        spin_on::spin_on(loader.import_component("std-widgets.slint", "Button", &mut import_diags))
            .expect("Button should import from a file-backed binary frozen artifact");
    assert!(!import_diags.has_errors());
    assert_eq!(button.id.as_str(), "Button");
}

#[test]
fn test_type_loader_new_uses_generated_builtin_artifact() {
    let mut artifact_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    artifact_config.style = Some("fluent".into());
    artifact_config.include_paths = vec![PathBuf::from("/__slint_generated_artifact_seed__")];
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, seed_loader) =
        spin_on::spin_on(crate::compile_syntax_node(doc_node, parse_diags, artifact_config));
    assert!(!compile_diags.has_errors());

    let frozen = seed_loader.freeze_builtin_semantic_metadata();
    let encoded = postcard::to_allocvec(&frozen).expect("frozen artifact should serialize");
    let static_artifact: &'static [u8] = Box::leak(encoded.into_boxed_slice());

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    compiler_config.translation_domain =
        Some(format!("generated-artifact-test-{}", std::process::id()));
    let key = TypeLoader::builtin_semantic_cache_key_for(&compiler_config, "fluent")
        .expect("generated artifact test key should be cacheable");
    crate::frozen_builtins::store_generated_artifact(key, static_artifact);

    let mut loader_diags = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut loader_diags);
    assert!(!loader_diags.has_errors());
    assert!(loader.get_document(Path::new("builtin:/fluent/std-widgets.slint")).is_some());

    let mut import_diags = BuildDiagnostics::default();
    let button =
        spin_on::spin_on(loader.import_component("std-widgets.slint", "Button", &mut import_diags))
            .expect("Button should import from the generated artifact fallback");
    assert!(!import_diags.has_errors());
    assert_eq!(button.id.as_str(), "Button");
}

#[test]
fn test_type_loader_new_uses_compiled_generated_builtin_artifact_if_present() {
    if crate::frozen_builtins::generated_artifact_count() == 0 {
        return;
    }

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    let mut loader_diags = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut loader_diags);
    assert!(!loader_diags.has_errors());
    assert!(loader.get_document(Path::new("builtin:/fluent/std-widgets.slint")).is_some());

    let mut import_diags = BuildDiagnostics::default();
    let button =
        spin_on::spin_on(loader.import_component("std-widgets.slint", "Button", &mut import_diags))
            .expect("Button should import from the compiled generated artifact");
    assert!(!import_diags.has_errors());
    assert_eq!(button.id.as_str(), "Button");
}

#[test]
fn test_type_loader_new_uses_compiled_generated_builtin_alias_artifact_if_present() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent-dark".into());
    let key = TypeLoader::builtin_semantic_cache_key_for(&compiler_config, "fluent-dark")
        .expect("generated artifact alias test key should be cacheable");
    if crate::frozen_builtins::generated_artifact(&key).is_none() {
        return;
    }

    let mut loader_diags = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut loader_diags);
    assert!(!loader_diags.has_errors());
    assert!(loader.get_document(Path::new("builtin:/fluent/style-base.slint")).is_some());

    let mut import_diags = BuildDiagnostics::default();
    let button =
        spin_on::spin_on(loader.import_component("std-widgets.slint", "Button", &mut import_diags))
            .expect("Button should import from the compiled generated alias artifact");
    assert!(!import_diags.has_errors());
    assert_eq!(button.id.as_str(), "Button");
}

#[test]
fn test_frozen_builtin_generated_artifact_table_is_compiled_in() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    let key = TypeLoader::builtin_semantic_cache_key_for(&compiler_config, "fluent")
        .expect("generated artifact table test key should be cacheable");

    assert_eq!(crate::frozen_builtins::generated_artifact_count(), 0);
    assert!(crate::frozen_builtins::generated_artifact(&key).is_none());
}

#[test]
fn test_frozen_builtin_artifact_emits_generated_rust_module() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, loader) = spin_on::spin_on(crate::compile_syntax_node(
        doc_node,
        parse_diags,
        compiler_config.clone(),
    ));
    assert!(!compile_diags.has_errors());

    let key = TypeLoader::builtin_semantic_cache_key_for(&compiler_config, "fluent")
        .expect("generated artifact emitter test key should be cacheable");
    let encoded =
        postcard::to_allocvec(&loader.freeze_builtin_semantic_metadata()).expect("encode failed");
    let source =
        crate::frozen_builtins::render_generated_artifacts_module(&[(key, encoded.clone())]);

    assert!(source.contains("static ARTIFACT_0"));
    assert!(source.contains("pub(crate) fn generated_artifact"));
    assert!(source.contains("key.resolved_style == \"fluent\""));
    assert!(source.contains("super::FrozenDefaultTranslationContext::ComponentName"));
    assert!(source.contains("pub(crate) fn artifact_count() -> usize {\n    1\n}"));
    assert!(source.len() > encoded.len());

    let path = std::env::temp_dir()
        .join(format!("slint-frozen-builtin-artifact-module-{}.rs", std::process::id()));
    std::fs::write(&path, &source).expect("generated artifact module should be written");
    let written = std::fs::read_to_string(&path).expect("generated artifact module should be read");
    std::fs::remove_file(&path).ok();
    assert_eq!(written, source);
}

#[test]
fn test_frozen_builtin_artifact_emits_out_dir_include_module() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    let key = TypeLoader::builtin_semantic_cache_key_for(&compiler_config, "fluent")
        .expect("generated artifact include module test key should be cacheable");
    let source = crate::frozen_builtins::render_generated_artifacts_include_module(&[(
        key,
        PathBuf::from("/tmp/slint-frozen-builtin-artifacts/fluent.postcard"),
    )]);

    assert!(source.contains(
        "static ARTIFACT_0: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/fluent.postcard\"));"
    ));
    assert!(!source.contains("/tmp/slint-frozen-builtin-artifacts/fluent.postcard"));
    assert!(source.contains("key.resolved_style == \"fluent\""));
    assert!(source.contains("pub(crate) fn artifact_count() -> usize {\n    1\n}"));
}

#[test]
#[cfg(feature = "frozen-builtin-artifacts")]
fn test_frozen_builtin_artifact_file_generation_sorts_and_deduplicates_styles() {
    let artifact_dir = std::env::temp_dir()
        .join(format!("slint-frozen-builtin-artifact-deterministic-{}", std::process::id()));
    std::fs::remove_dir_all(&artifact_dir).ok();

    let module_path = TypeLoader::generate_frozen_builtin_artifact_files(
        &["material".into(), "fluent".into(), "fluent".into()],
        &artifact_dir,
    )
    .expect("generated artifact files should be written");
    assert_eq!(module_path, artifact_dir.join("frozen_builtin_artifacts.rs"));

    let manifest = std::fs::read_to_string(artifact_dir.join("frozen_builtin_artifacts.manifest"))
        .expect("generated artifact manifest should be readable");
    assert!(manifest.contains("artifact_count=2"));
    let fluent_index = manifest.find("style=fluent").expect("fluent artifact should be listed");
    let material_index =
        manifest.find("style=material").expect("material artifact should be listed");
    assert!(fluent_index < material_index);

    let module = std::fs::read_to_string(module_path).expect("generated module should be readable");
    assert!(module.contains("/fluent.postcard"));
    assert!(module.contains("/material.postcard"));

    std::fs::remove_dir_all(&artifact_dir).ok();
}

#[test]
fn test_frozen_builtin_skeletons_rehydrate_into_registry() {
    let compiler_config = CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    let source = r#"
        import { Button } from "std-widgets.slint";
        export component Test inherits Button {}
    "#;

    let mut parse_diags = BuildDiagnostics::default();
    let doc_node = crate::parser::parse(source.into(), None, &mut parse_diags);
    let (_doc, compile_diags, loader) =
        spin_on::spin_on(crate::compile_syntax_node(doc_node, parse_diags, compiler_config));
    assert!(!compile_diags.has_errors());

    let frozen = loader.freeze_builtin_semantic_metadata();
    let component = frozen
        .documents
        .iter()
        .flat_map(|doc| &doc.components)
        .find(|component| !component.root_element.children.is_empty())
        .expect("expected at least one builtin component skeleton");
    let component_id = component.id.clone();

    let parent_registry = TypeRegister::builtin();
    let rehydrated = frozen.rehydrate_component_skeletons(&parent_registry);
    let rehydrated_component = match rehydrated.lookup_element(&component_id) {
        Ok(langtype::ElementType::Component(component)) => component,
        other => panic!("expected rehydrated component {component_id}, got {other:?}"),
    };

    assert_eq!(rehydrated_component.id.as_str(), component_id);
    assert!(!rehydrated_component.root_element.borrow().children.is_empty());
}

#[test]
fn test_load_from_callback_ok() {
    let ok = Rc::new(core::cell::Cell::new(false));
    let ok_ = ok.clone();

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    compiler_config.open_import_callback = Some(Rc::new(move |path| {
        let ok_ = ok_.clone();
        Box::pin(async move {
            assert_eq!(path.replace('\\', "/"), "../FooBar.slint");
            assert!(!ok_.get());
            ok_.set(true);
            Some(Ok("export XX := Rectangle {} ".to_owned()))
        })
    }));

    let mut test_diags = crate::diagnostics::BuildDiagnostics::default();
    let doc_node = crate::parser::parse(
        r#"
/* ... */
import { XX } from "../Ab/.././FooBar.slint";
X := XX {}
"#
        .into(),
        Some(std::path::Path::new("HELLO")),
        &mut test_diags,
    );

    let doc_node: syntax_nodes::Document = doc_node.into();
    let mut build_diagnostics = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    let registry = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));
    spin_on::spin_on(loader.load_dependencies_recursively(
        &doc_node,
        &mut build_diagnostics,
        &registry,
    ));
    assert!(ok.get());
    assert!(!test_diags.has_errors());
    assert!(!build_diagnostics.has_errors());
}

#[test]
fn test_load_error_twice() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    let mut test_diags = crate::diagnostics::BuildDiagnostics::default();

    let doc_node = crate::parser::parse(
        r#"
/* ... */
import { XX } from "error.slint";
component Foo { XX {} }
"#
        .into(),
        Some(std::path::Path::new("HELLO")),
        &mut test_diags,
    );

    let doc_node: syntax_nodes::Document = doc_node.into();
    let mut build_diagnostics = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    let registry = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));
    spin_on::spin_on(loader.load_dependencies_recursively(
        &doc_node,
        &mut build_diagnostics,
        &registry,
    ));
    assert!(!test_diags.has_errors());
    assert!(build_diagnostics.has_errors());
    let diags = build_diagnostics.to_string_vec();
    assert_eq!(
        diags,
        &["HELLO:3: Cannot find requested import \"error.slint\" in the include search path"]
    );
    // Try loading another time with the same registry
    let mut build_diagnostics = BuildDiagnostics::default();
    spin_on::spin_on(loader.load_dependencies_recursively(
        &doc_node,
        &mut build_diagnostics,
        &registry,
    ));
    assert!(build_diagnostics.has_errors());
    let diags = build_diagnostics.to_string_vec();
    assert_eq!(
        diags,
        &["HELLO:3: Cannot find requested import \"error.slint\" in the include search path"]
    );
}

#[test]
fn test_load_file_watches_missing_imports() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    compiler_config.embed_resources = crate::EmbedResourcesKind::ListAllResources;
    let mut build_diagnostics = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    let main_path = Path::new("/tmp/main.slint");

    spin_on::spin_on(
        loader.load_file(
            main_path,
            main_path,
            r#"
/* ... */
import { XX } from "missing/dependency.slint";
component Foo { XX {} }
"#
            .into(),
            false,
            &mut build_diagnostics,
        ),
    );

    assert!(build_diagnostics.has_errors());

    let watch_files = loader.all_files_to_watch();
    assert!(watch_files.contains(&PathBuf::from("/tmp/main.slint")));
    assert!(watch_files.contains(&PathBuf::from("/tmp/missing/dependency.slint")));
}

#[test]
fn test_load_root_file_tracks_missing_imports() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    compiler_config.embed_resources = crate::EmbedResourcesKind::ListAllResources;
    let mut build_diagnostics = BuildDiagnostics::default();
    let main_path = std::env::temp_dir().join("main.slint");
    let missing_path = main_path.with_file_name("missing.slint");
    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    spin_on::spin_on(
        loader.load_root_file(
            &main_path,
            &main_path,
            r#"
import { Missing } from "missing.slint";
export component Main inherits Window {
    Missing { }
}
"#
            .into(),
            false,
            &mut build_diagnostics,
        ),
    );

    assert!(build_diagnostics.has_errors());
    assert!(
        loader.all_files_to_watch().contains(&main_path),
        "watch paths: {:?}",
        loader.all_files_to_watch()
    );
    assert!(
        loader.all_files_to_watch().contains(&missing_path),
        "watch paths: {:?}",
        loader.all_files_to_watch()
    );
}

#[test]
fn test_load_root_file_tracks_missing_resources() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    compiler_config.embed_resources = crate::EmbedResourcesKind::ListAllResources;
    let mut build_diagnostics = BuildDiagnostics::default();
    let main_path = std::env::temp_dir().join("main.slint");
    let resource_path = main_path.with_file_name("icon.svg");

    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    spin_on::spin_on(
        loader.load_root_file(
            &main_path,
            &main_path,
            r#"
export component Main inherits Window {
    Image {
        source: @image-url("icon.svg");
    }
}
"#
            .into(),
            false,
            &mut build_diagnostics,
        ),
    );

    // The image is not embedded, so it doesn't cause an error
    assert!(!build_diagnostics.has_errors());
    assert!(
        loader.all_files_to_watch().contains(&main_path),
        "watch paths: {:?}",
        loader.all_files_to_watch()
    );
    assert!(
        loader.all_files_to_watch().contains(&resource_path),
        "watch paths: {:?}",
        loader.all_files_to_watch()
    );
}

#[test]
fn test_manual_import() {
    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.style = Some("fluent".into());
    let mut build_diagnostics = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);

    let maybe_button_type = spin_on::spin_on(loader.import_component(
        "std-widgets.slint",
        "Button",
        &mut build_diagnostics,
    ));

    assert!(!build_diagnostics.has_errors());
    assert!(maybe_button_type.is_some());
}

#[test]
fn test_builtin_style() {
    let test_source_path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "typeloader"].iter().collect();

    let incdir = test_source_path.join("custom_style");

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.include_paths = vec![incdir];
    compiler_config.style = Some("fluent".into());

    let mut build_diagnostics = BuildDiagnostics::default();
    let _loader = TypeLoader::new(compiler_config, &mut build_diagnostics);

    assert!(!build_diagnostics.has_errors());
}

#[test]
fn test_user_style() {
    let test_source_path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "typeloader"].iter().collect();

    let incdir = test_source_path.join("custom_style");

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.include_paths = vec![incdir];
    compiler_config.style = Some("TestStyle".into());

    let mut build_diagnostics = BuildDiagnostics::default();
    let _loader = TypeLoader::new(compiler_config, &mut build_diagnostics);

    assert!(!build_diagnostics.has_errors());
}

#[test]
fn test_unknown_style() {
    let test_source_path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "typeloader"].iter().collect();

    let incdir = test_source_path.join("custom_style");

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.include_paths = vec![incdir];
    compiler_config.style = Some("FooBar".into());

    let mut build_diagnostics = BuildDiagnostics::default();
    let _loader = TypeLoader::new(compiler_config, &mut build_diagnostics);

    assert!(build_diagnostics.has_errors());
    let diags = build_diagnostics.to_string_vec();
    assert_eq!(diags.len(), 1);
    assert!(diags[0].starts_with("Style FooBar is not known. Use one of the builtin styles ["));
}

#[test]
fn test_library_import() {
    let test_source_path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "typeloader", "library"].iter().collect();

    let library_paths = HashMap::from([
        ("libdir".into(), test_source_path.clone()),
        ("libfile.slint".into(), test_source_path.join("lib.slint")),
    ]);

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.library_paths = library_paths;
    compiler_config.style = Some("fluent".into());
    let mut test_diags = crate::diagnostics::BuildDiagnostics::default();

    let doc_node = crate::parser::parse(
        r#"
/* ... */
import { LibraryType } from "@libfile.slint";
import { LibraryHelperType } from "@libdir/library_helper_type.slint";
"#
        .into(),
        Some(std::path::Path::new("HELLO")),
        &mut test_diags,
    );

    let doc_node: syntax_nodes::Document = doc_node.into();
    let mut build_diagnostics = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    let registry = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));
    spin_on::spin_on(loader.load_dependencies_recursively(
        &doc_node,
        &mut build_diagnostics,
        &registry,
    ));
    assert!(!test_diags.has_errors());
    assert!(!build_diagnostics.has_errors());
}

#[test]
fn test_library_import_errors() {
    let test_source_path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "typeloader", "library"].iter().collect();

    let library_paths = HashMap::from([
        ("libdir".into(), test_source_path.clone()),
        ("libfile.slint".into(), test_source_path.join("lib.slint")),
    ]);

    let mut compiler_config =
        CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter);
    compiler_config.library_paths = library_paths;
    compiler_config.style = Some("fluent".into());
    let mut test_diags = crate::diagnostics::BuildDiagnostics::default();

    let doc_node = crate::parser::parse(
        r#"
/* ... */
import { A } from "@libdir";
import { B } from "@libdir/unknown.slint";
import { C } from "@libfile.slint/unknown.slint";
import { D } from "@unknown";
import { E } from "@unknown/lib.slint";
"#
        .into(),
        Some(std::path::Path::new("HELLO")),
        &mut test_diags,
    );

    let doc_node: syntax_nodes::Document = doc_node.into();
    let mut build_diagnostics = BuildDiagnostics::default();
    let mut loader = TypeLoader::new(compiler_config, &mut build_diagnostics);
    let registry = Rc::new(RefCell::new(TypeRegister::new(&loader.global_type_registry)));
    spin_on::spin_on(loader.load_dependencies_recursively(
        &doc_node,
        &mut build_diagnostics,
        &registry,
    ));
    assert!(!test_diags.has_errors());
    assert!(build_diagnostics.has_errors());
    let diags = build_diagnostics.to_string_vec();
    assert_eq!(diags.len(), 5);
    assert_starts_with(
        &diags[0],
        &format!(
            "HELLO:3: Error reading requested import \"{}\": ",
            test_source_path.to_string_lossy()
        ),
    );
    assert_starts_with(
        &diags[1],
        &format!(
            "HELLO:4: Error reading requested import \"{}\": ",
            test_source_path.join("unknown.slint").to_string_lossy(),
        ),
    );
    assert_starts_with(
        &diags[2],
        &format!(
            "HELLO:5: Error reading requested import \"{}\": ",
            test_source_path.join("lib.slint").join("unknown.slint").to_string_lossy()
        ),
    );
    assert_eq!(
        &diags[3],
        "HELLO:6: Cannot find requested import \"@unknown\" in the library search path"
    );
    assert_eq!(
        &diags[4],
        "HELLO:7: Cannot find requested import \"@unknown/lib.slint\" in the library search path"
    );

    #[track_caller]
    fn assert_starts_with(actual: &str, start: &str) {
        assert!(actual.starts_with(start), "{actual:?} does not start with {start:?}");
    }
}

#[test]
fn test_snapshotting() {
    let mut type_loader = TypeLoader::new(
        crate::CompilerConfiguration::new(crate::generator::OutputFormat::Interpreter),
        &mut BuildDiagnostics::default(),
    );

    let path = PathBuf::from("/tmp/test.slint");
    let mut diag = BuildDiagnostics::default();
    spin_on::spin_on(type_loader.load_file(
        &path,
        &path,
        "export component Foobar inherits Rectangle { }".to_string(),
        false,
        &mut diag,
    ));

    assert!(!diag.has_errors());

    let doc = type_loader.get_document(&path).unwrap();
    let c = doc.inner_components.first().unwrap();
    assert_eq!(c.id, "Foobar");
    let root_element = c.root_element.clone();
    assert_eq!(root_element.borrow().base_type.to_string(), "Rectangle");

    let copy = snapshot(&type_loader).unwrap();

    let doc = copy.get_document(&path).unwrap();
    let c = doc.inner_components.first().unwrap();
    assert_eq!(c.id, "Foobar");
    let root_element = c.root_element.clone();
    assert_eq!(root_element.borrow().base_type.to_string(), "Rectangle");
}
