// import maps as the HTML standard defines them: bare specifiers, and URL prefixes, mapped
// to addresses, with scopes that apply to modules under a path

use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImportMap {
    imports: Vec<(String, String)>,
    // scope prefix and its own mappings, longest prefix first
    scopes: Vec<(String, Vec<(String, String)>)>,
}

fn is_url_like(specifier: &str) -> bool {
    specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.contains("://")
}

// the entries of a JSON object in source order
fn object_entries(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Object(object) => Some(object.borrow().iter().collect()),
        _ => None,
    }
}

// a mapping table with addresses resolved against the map's own URL, longest key first
fn mappings(value: Option<&Value>, base: &AppUrl) -> Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries =
        object_entries(value).ok_or_else(|| "import map mappings must be an object".to_string())?;
    let mut table = Vec::new();
    for (key, address) in &entries {
        let Value::String(address) = address else {
            continue;
        };
        let address: &str = address;
        if key.ends_with('/') != address.ends_with('/') {
            continue;
        }
        let Ok(url) = base.resolve(address) else {
            continue;
        };
        let key = if is_url_like(key) {
            match base.resolve(key) {
                Ok(url) => url.to_string(),
                Err(_) => continue,
            }
        } else {
            key.clone()
        };
        let mut resolved = url.to_string();
        if address.ends_with('/') && !resolved.ends_with('/') {
            resolved.push('/');
        }
        table.push((key, resolved));
    }
    table.sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
    Ok(table)
}

fn apply(table: &[(String, String)], specifier: &str) -> Option<String> {
    table.iter().find_map(|(key, address)| {
        if key == specifier {
            Some(address.clone())
        } else if key.ends_with('/') {
            specifier
                .strip_prefix(key.as_str())
                .map(|rest| format!("{address}{rest}"))
        } else {
            None
        }
    })
}

impl ImportMap {
    pub(crate) fn parse(source: &str, base: &AppUrl) -> Result<Self, String> {
        let json = super::json::parse(source)?;
        let entries = object_entries(&json)
            .ok_or_else(|| "an import map must be a JSON object".to_string())?;
        let field = |name: &str| {
            entries
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let imports = mappings(field("imports"), base)?;
        let mut scopes = Vec::new();
        if let Some(scope_entries) = field("scopes").and_then(object_entries) {
            for (prefix, table) in &scope_entries {
                let Ok(prefix) = base.resolve(prefix) else {
                    continue;
                };
                scopes.push((prefix.to_string(), mappings(Some(table), base)?));
            }
        }
        scopes.sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
        Ok(Self { imports, scopes })
    }

    // merges a later map in; entries already present keep their first meaning
    pub(crate) fn merge(&mut self, other: Self) {
        for (key, address) in other.imports {
            if !self.imports.iter().any(|(existing, _)| *existing == key) {
                self.imports.push((key, address));
            }
        }
        self.imports
            .sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
        for (prefix, table) in other.scopes {
            if !self.scopes.iter().any(|(existing, _)| *existing == prefix) {
                self.scopes.push((prefix, table));
            }
        }
        self.scopes
            .sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
    }

    // the address a specifier imported from the referrer maps to, if the map names it
    pub(crate) fn resolve(&self, specifier: &str, referrer: &AppUrl) -> Option<String> {
        let key = if is_url_like(specifier) {
            referrer.resolve(specifier).ok()?.to_string()
        } else {
            specifier.to_string()
        };
        let referrer = referrer.to_string();
        self.scopes
            .iter()
            .filter(|(prefix, _)| {
                referrer == *prefix || (prefix.ends_with('/') && referrer.starts_with(prefix))
            })
            .find_map(|(_, table)| apply(table, &key))
            .or_else(|| apply(&self.imports, &key))
    }
}
