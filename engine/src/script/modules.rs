// ES module graphs in the standard's three steps: every module in the graph is instantiated
// with its export bindings first, imports and re-exports are then resolved against those
// bindings (so import cycles link), and modules run after their dependencies

use super::compiler::ModuleCode;
use super::*;

struct ModuleRecord {
    url: AppUrl,
    module: ModuleCode,
    scope: vm::ScopeRef,
    // the module's own exports, as binding cells in its scope
    locals: BTreeMap<String, Value>,
    // each specifier this module requested, in source order, with the module it named
    dependencies: Vec<(String, String)>,
    evaluated: bool,
}

impl ModuleRecord {
    fn dependency(&self, specifier: &str) -> Option<&str> {
        self.dependencies
            .iter()
            .find(|(requested, _)| requested == specifier)
            .map(|(_, key)| key.as_str())
    }
}

#[derive(Default)]
pub(super) struct ModuleGraph {
    records: HashMap<String, ModuleRecord>,
    // keys in the order they were instantiated
    order: Vec<String>,
}

impl ScriptRuntime {
    pub(super) fn execute_module_graph<L: ModuleSourceLoader>(
        &mut self,
        source: &str,
        url: AppUrl,
        loader: &mut L,
    ) -> Result<ModuleNamespace, ScriptError> {
        let key = url.to_string();
        if let Some(namespace) = self.module_cache.get(&key) {
            return Ok(namespace.clone());
        }
        let mut graph = ModuleGraph::default();
        self.instantiate(&mut graph, source, url, loader)?;
        self.link(&mut graph)?;
        let keys = graph.order.clone();
        for current in &keys {
            let namespace = ModuleNamespace {
                values: self.module_exports(&graph, current, &mut Vec::new())?,
            };
            self.module_cache.insert(current.clone(), namespace);
        }
        // a module graph that fails to run leaves nothing behind for a later import to find
        if let Err(error) = self.evaluate(&mut graph, &key) {
            for current in &keys {
                self.module_cache.remove(current);
            }
            return Err(error);
        }
        Ok(self.module_cache[&key].clone())
    }

    fn instantiate<L: ModuleSourceLoader>(
        &mut self,
        graph: &mut ModuleGraph,
        source: &str,
        url: AppUrl,
        loader: &mut L,
    ) -> Result<(), ScriptError> {
        let key = url.to_string();
        let module = parser::parse_module(source)?;
        let module = compiler::Compiler::new(source).compile_module(&module)?;
        let scope = vm::Scope::new(module.code.scope_size as usize, None);
        let mut locals = BTreeMap::new();
        for (exported, slot) in &module.exports {
            let cell = Rc::new(RefCell::new(Value::Undefined));
            scope.borrow_mut().slots[*slot as usize] = Value::Binding(cell.clone(), true);
            locals.insert(exported.to_string(), Value::Binding(cell, false));
        }
        let specifiers = module
            .imports
            .iter()
            .map(|(specifier, _)| specifier.clone())
            .chain(module.reexports.iter().map(|(from, _, _)| from.clone()))
            .chain(
                module
                    .namespace_exports
                    .iter()
                    .map(|(from, _)| from.clone()),
            )
            .chain(module.star_exports.iter().cloned())
            .collect::<Vec<_>>();
        graph.order.push(key.clone());
        graph.records.insert(
            key.clone(),
            ModuleRecord {
                url: url.clone(),
                module,
                scope,
                locals,
                dependencies: Vec::new(),
                evaluated: false,
            },
        );
        for specifier in specifiers {
            let mapped = self.import_map.resolve(&specifier, &url);
            let (dependency_url, dependency_source) =
                loader.load_module(&url, mapped.as_deref().unwrap_or(&specifier))?;
            let dependency = dependency_url.to_string();
            if let Some(record) = graph.records.get_mut(&key) {
                record
                    .dependencies
                    .push((specifier.to_string(), dependency.clone()));
            }
            if !graph.records.contains_key(&dependency)
                && !self.module_cache.contains_key(&dependency)
            {
                self.instantiate(graph, &dependency_source, dependency_url, loader)?;
            }
        }
        Ok(())
    }

    // the binding a module exports under a name, following re-exports and star exports
    fn resolve_export(
        &self,
        graph: &ModuleGraph,
        key: &str,
        name: &str,
        visiting: &mut Vec<(String, String)>,
    ) -> Option<Value> {
        if let Some(namespace) = self.module_cache.get(key) {
            return namespace.values.get(name).cloned();
        }
        let visit = (key.to_string(), name.to_string());
        if visiting.contains(&visit) {
            return None;
        }
        visiting.push(visit);
        let record = graph.records.get(key)?;
        if let Some(local) = record.locals.get(name) {
            return Some(local.clone());
        }
        for (from, imported, exported) in &record.module.reexports {
            if &**exported == name {
                let source = record.dependency(from)?;
                return self.resolve_export(graph, source, imported, visiting);
            }
        }
        for (from, alias) in &record.module.namespace_exports {
            if &**alias == name {
                let source = record.dependency(from)?.to_string();
                let values = self.module_exports(graph, &source, visiting).ok()?;
                return Some(self.namespace_object(&ModuleNamespace { values }));
            }
        }
        if name == "default" {
            return None;
        }
        record.module.star_exports.iter().find_map(|from| {
            let source = record.dependency(from)?;
            self.resolve_export(graph, source, name, visiting)
        })
    }

    // every name a module exports, with its binding
    fn module_exports(
        &self,
        graph: &ModuleGraph,
        key: &str,
        visiting: &mut Vec<(String, String)>,
    ) -> Result<BTreeMap<String, Value>, ScriptError> {
        if let Some(namespace) = self.module_cache.get(key) {
            return Ok(namespace.values.clone());
        }
        let Some(record) = graph.records.get(key) else {
            return Ok(BTreeMap::new());
        };
        let mut names = record.locals.keys().cloned().collect::<Vec<_>>();
        names.extend(
            record
                .module
                .reexports
                .iter()
                .map(|(_, _, exported)| exported.to_string()),
        );
        names.extend(
            record
                .module
                .namespace_exports
                .iter()
                .map(|(_, alias)| alias.to_string()),
        );
        let marker = (key.to_string(), "*".to_string());
        if !visiting.contains(&marker) {
            visiting.push(marker);
            for from in &record.module.star_exports {
                if let Some(source) = record.dependency(from) {
                    let starred = self.module_exports(graph, source, visiting)?;
                    names.extend(starred.into_keys().filter(|name| name != "default"));
                }
            }
        }
        let mut values = BTreeMap::new();
        for name in names {
            if values.contains_key(&name) {
                continue;
            }
            if let Some(value) = self.resolve_export(graph, key, &name, &mut Vec::new()) {
                values.insert(name, value);
            }
        }
        Ok(values)
    }

    // fills every import slot from the bindings the graph exports
    fn link(&mut self, graph: &mut ModuleGraph) -> Result<(), ScriptError> {
        for key in graph.order.clone() {
            let mut slots = Vec::new();
            {
                let record = &graph.records[&key];
                for (specifier, bindings) in &record.module.imports {
                    let source = record
                        .dependency(specifier)
                        .expect("every requested module was instantiated")
                        .to_string();
                    for (binding, slot) in bindings {
                        let value = match binding {
                            ast::ImportBinding::Named { imported, .. } => self
                                .resolve_export(graph, &source, imported, &mut Vec::new())
                                .ok_or_else(|| {
                                    runtime_error(format!(
                                        "module {specifier} has no export {imported}"
                                    ))
                                })?,
                            ast::ImportBinding::Namespace(_) => {
                                let values =
                                    self.module_exports(graph, &source, &mut Vec::new())?;
                                self.namespace_object(&ModuleNamespace { values })
                            }
                        };
                        let value = match value {
                            Value::Binding(cell, _) => Value::Binding(cell, false),
                            value => value,
                        };
                        slots.push((*slot, value));
                    }
                }
            }
            let record = graph.records.get_mut(&key).expect("record exists");
            let mut scope = record.scope.borrow_mut();
            for (slot, value) in slots {
                // an import that is exported again keeps the binding it was linked to
                for (exported, export_slot) in &record.module.exports {
                    if *export_slot == slot {
                        record.locals.insert(exported.to_string(), value.clone());
                    }
                }
                scope.slots[slot as usize] = value;
            }
        }
        Ok(())
    }

    // runs a module after its dependencies; a module already running in a cycle is skipped
    fn evaluate(&mut self, graph: &mut ModuleGraph, key: &str) -> Result<(), ScriptError> {
        let Some(record) = graph.records.get_mut(key) else {
            return Ok(());
        };
        if record.evaluated {
            return Ok(());
        }
        record.evaluated = true;
        let dependencies = record
            .dependencies
            .iter()
            .map(|(_, key)| key.clone())
            .collect::<Vec<_>>();
        for dependency in dependencies {
            self.evaluate(graph, &dependency)?;
        }
        let record = &graph.records[key];
        let (code, scope, url) = (
            record.module.code.clone(),
            record.scope.clone(),
            record.url.clone(),
        );
        self.run_module_code(code, scope, url)?;
        Ok(())
    }
}
