use std::collections::HashMap;
use std::sync::Arc;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameId(pub u32);

/// `self` is always the first interned name.
pub const SELF_NAME: NameId = NameId(0);

#[derive(Clone, Debug)]
pub struct NameTable {
    entries: Vec<Arc<str>>,
    map: HashMap<Arc<str>, NameId>,
}

impl Default for NameTable {
    fn default() -> Self {
        let mut table = Self {
            entries: Vec::new(),
            map: HashMap::new(),
        };
        table.intern("self");
        table
    }
}

impl NameTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, text: &str) -> NameId {
        if let Some(&id) = self.map.get(text) {
            return id;
        }
        let id = NameId(self.entries.len() as u32);
        let text: Arc<str> = Arc::from(text);
        self.map.insert(Arc::clone(&text), id);
        self.entries.push(text);
        id
    }

    pub fn name(&self, id: NameId) -> &str {
        &self.entries[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
