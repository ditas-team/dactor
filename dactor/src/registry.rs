use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Thread-safe registry for looking up actors by name.
///
/// Actors are registered at spawn time and can be looked up by name
/// from any task. The registry stores type-erased refs; callers must
/// know the actor type to downcast.
///
/// **Lifecycle:** Actors are auto-registered on spawn but NOT auto-unregistered
/// on stop. Call [`unregister`](Self::unregister) explicitly when actors
/// terminate, or accept that stopped actors remain in the registry (their
/// refs will report `is_alive() == false`).
pub struct ActorRegistry {
    entries: Mutex<HashMap<String, Arc<dyn Any + Send + Sync>>>,
}

impl ActorRegistry {
    /// Create a new empty actor registry.
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Register an actor ref under a name. Overwrites if name exists.
    /// The ref must be `Clone` so it can be retrieved via [`lookup`](Self::lookup).
    pub fn register<R: Clone + Send + Sync + 'static>(&self, name: &str, actor_ref: R) {
        let boxed: Arc<dyn Any + Send + Sync> = Arc::new(actor_ref);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.insert(name.to_string(), boxed);
    }

    /// Look up an actor by name, returning a clone of its ref.
    /// Returns `None` if not found or if the type doesn't match.
    pub fn lookup<R: Clone + 'static>(&self, name: &str) -> Option<R> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(name)
            .and_then(|entry| entry.downcast_ref::<R>())
            .cloned()
    }

    /// Remove an actor from the registry.
    pub fn unregister(&self, name: &str) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .is_some()
    }

    /// Check if a name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(name)
    }

    /// Number of registered actors.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// List all registered names.
    pub fn names(&self) -> Vec<String> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

impl Default for ActorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let registry = ActorRegistry::new();
        registry.register("greeting", "hello".to_string());
        assert_eq!(
            registry.lookup::<String>("greeting"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn lookup_wrong_type_returns_none() {
        let registry = ActorRegistry::new();
        registry.register("num", 42u64);
        assert_eq!(registry.lookup::<String>("num"), None);
    }

    #[test]
    fn lookup_missing_name_returns_none() {
        let registry = ActorRegistry::new();
        assert_eq!(registry.lookup::<String>("missing"), None);
    }

    #[test]
    fn unregister_removes_entry() {
        let registry = ActorRegistry::new();
        registry.register("item", "value".to_string());
        assert!(registry.contains("item"));
        assert!(registry.unregister("item"));
        assert!(!registry.contains("item"));
        assert!(!registry.unregister("item"));
    }

    #[test]
    fn len_and_names() {
        let registry = ActorRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());

        registry.register("a", 1u32);
        registry.register("b", 2u32);
        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());

        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn overwrite_existing_name() {
        let registry = ActorRegistry::new();
        registry.register("key", "old".to_string());
        registry.register("key", "new".to_string());
        assert_eq!(registry.lookup::<String>("key"), Some("new".to_string()));
        assert_eq!(registry.len(), 1);
    }
}
