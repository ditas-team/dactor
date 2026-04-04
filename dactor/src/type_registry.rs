//! Type registry for remote message deserialization and actor factory lookup.
//!
//! When a node receives a [`WireEnvelope`](crate::remote::WireEnvelope) from the
//! network, it needs to deserialize the message body bytes back into a concrete
//! Rust type. The [`TypeRegistry`] maps message type names (strings) to
//! deserializer functions that know how to reconstruct the original type.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use dactor::type_registry::TypeRegistry;
//!
//! let mut registry = TypeRegistry::new();
//!
//! // Register a deserializer for a message type
//! registry.register("myapp::Increment", |bytes| {
//!     let msg: Increment = serde_json::from_slice(bytes)?;
//!     Ok(Box::new(msg))
//! });
//!
//! // Later, when receiving a WireEnvelope:
//! let any_msg = registry.deserialize("myapp::Increment", &body_bytes)?;
//! let msg = any_msg.downcast::<Increment>().unwrap();
//! ```

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use crate::remote::SerializationError;

/// A function that deserializes bytes into a type-erased value.
pub type DeserializerFn =
    Arc<dyn Fn(&[u8]) -> Result<Box<dyn Any + Send>, SerializationError> + Send + Sync>;

/// A function that creates an actor from serialized Args bytes.
/// Used for remote actor spawning.
pub type FactoryFn =
    Arc<dyn Fn(&[u8]) -> Result<Box<dyn Any + Send>, SerializationError> + Send + Sync>;

/// Registry mapping message type names to their deserializer functions.
///
/// Populated at startup with all message types that the node can handle
/// remotely. When a [`WireEnvelope`](crate::remote::WireEnvelope) arrives, the
/// registry is consulted to find the correct deserializer for the
/// `message_type` field.
pub struct TypeRegistry {
    /// Message type name → deserializer function.
    deserializers: HashMap<String, DeserializerFn>,
    /// Actor type name → factory function (for remote spawn).
    factories: HashMap<String, FactoryFn>,
}

impl TypeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            deserializers: HashMap::new(),
            factories: HashMap::new(),
        }
    }

    /// Register a deserializer function for a message type.
    ///
    /// The `type_name` should match the `message_type` field in
    /// [`WireEnvelope`](crate::remote::WireEnvelope), typically
    /// `std::any::type_name::<M>()`.
    pub fn register(
        &mut self,
        type_name: impl Into<String>,
        deserializer: impl Fn(&[u8]) -> Result<Box<dyn Any + Send>, SerializationError>
            + Send
            + Sync
            + 'static,
    ) {
        self.deserializers
            .insert(type_name.into(), Arc::new(deserializer));
    }

    /// Register a message type using serde_json deserialization.
    ///
    /// Convenience method that auto-generates a deserializer for types that
    /// implement `serde::Deserialize`. Uses `std::any::type_name::<M>()` as
    /// the registry key.
    #[cfg(feature = "serde")]
    pub fn register_type<M>(&mut self)
    where
        M: serde::de::DeserializeOwned + Send + 'static,
    {
        let type_name = std::any::type_name::<M>().to_string();
        self.deserializers.insert(
            type_name,
            Arc::new(|bytes: &[u8]| {
                let value: M = serde_json::from_slice(bytes).map_err(|e| {
                    SerializationError::new(format!(
                        "failed to deserialize {}: {e}",
                        std::any::type_name::<M>()
                    ))
                })?;
                Ok(Box::new(value) as Box<dyn Any + Send>)
            }),
        );
    }

    /// Deserialize a message body using the registered deserializer.
    ///
    /// Returns `Err` if no deserializer is registered for `type_name`.
    pub fn deserialize(
        &self,
        type_name: &str,
        bytes: &[u8],
    ) -> Result<Box<dyn Any + Send>, SerializationError> {
        let deserializer = self.deserializers.get(type_name).ok_or_else(|| {
            SerializationError::new(format!("no deserializer registered for '{type_name}'"))
        })?;
        deserializer(bytes)
    }

    /// Check if a deserializer is registered for a type name.
    pub fn has_type(&self, type_name: &str) -> bool {
        self.deserializers.contains_key(type_name)
    }

    /// Number of registered message types.
    pub fn type_count(&self) -> usize {
        self.deserializers.len()
    }

    /// Register an actor factory function for remote spawning.
    ///
    /// The `type_name` should match `std::any::type_name::<A>()` for
    /// the actor type. The factory deserializes the actor's `Args` from
    /// bytes and returns the constructed actor as `Box<dyn Any + Send>`.
    pub fn register_factory(
        &mut self,
        type_name: impl Into<String>,
        factory: impl Fn(&[u8]) -> Result<Box<dyn Any + Send>, SerializationError>
            + Send
            + Sync
            + 'static,
    ) {
        self.factories.insert(type_name.into(), Arc::new(factory));
    }

    /// Register an actor factory using serde_json for `Args` deserialization.
    ///
    /// Convenience method for actors whose `Args` type implements
    /// `serde::Deserialize`. The factory deserializes `Args`, then calls
    /// `Actor::create(args, deps)` with the provided `deps`.
    #[cfg(feature = "serde")]
    pub fn register_actor<A>(&mut self, deps: A::Deps)
    where
        A: crate::actor::Actor + Send + 'static,
        A::Args: serde::de::DeserializeOwned,
        A::Deps: Clone + Send + Sync + 'static,
    {
        let type_name = std::any::type_name::<A>().to_string();
        self.factories.insert(
            type_name,
            Arc::new(move |bytes: &[u8]| {
                let args: A::Args = serde_json::from_slice(bytes).map_err(|e| {
                    SerializationError::new(format!(
                        "failed to deserialize args for {}: {e}",
                        std::any::type_name::<A>()
                    ))
                })?;
                let actor = A::create(args, deps.clone());
                Ok(Box::new(actor) as Box<dyn Any + Send>)
            }),
        );
    }

    /// Look up an actor factory by type name and create an actor from bytes.
    pub fn create_actor(
        &self,
        type_name: &str,
        args_bytes: &[u8],
    ) -> Result<Box<dyn Any + Send>, SerializationError> {
        let factory = self.factories.get(type_name).ok_or_else(|| {
            SerializationError::new(format!("no actor factory registered for '{type_name}'"))
        })?;
        factory(args_bytes)
    }

    /// Check if a factory is registered for an actor type name.
    pub fn has_factory(&self, type_name: &str) -> bool {
        self.factories.contains_key(type_name)
    }

    /// Number of registered actor factories.
    pub fn factory_count(&self) -> usize {
        self.factories.len()
    }
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_deserialize_custom() {
        let mut registry = TypeRegistry::new();

        // Register a simple u64 deserializer (big-endian 8 bytes)
        registry.register("test::Counter", |bytes: &[u8]| {
            if bytes.len() != 8 {
                return Err(SerializationError::new("expected 8 bytes"));
            }
            let val = u64::from_be_bytes(bytes.try_into().unwrap());
            Ok(Box::new(val))
        });

        assert!(registry.has_type("test::Counter"));
        assert!(!registry.has_type("test::Unknown"));
        assert_eq!(registry.type_count(), 1);

        // Deserialize
        let bytes = 42u64.to_be_bytes();
        let any = registry.deserialize("test::Counter", &bytes).unwrap();
        let val = any.downcast::<u64>().unwrap();
        assert_eq!(*val, 42);
    }

    #[test]
    fn deserialize_unknown_type_returns_error() {
        let registry = TypeRegistry::new();
        let result = registry.deserialize("unknown::Type", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("no deserializer"));
    }

    #[test]
    fn register_and_create_actor_custom() {
        let mut registry = TypeRegistry::new();

        // Register a factory that parses a u64 "args" and returns it
        registry.register_factory("test::Worker", |bytes: &[u8]| {
            if bytes.len() != 8 {
                return Err(SerializationError::new("expected 8 bytes"));
            }
            let val = u64::from_be_bytes(bytes.try_into().unwrap());
            Ok(Box::new(val))
        });

        assert!(registry.has_factory("test::Worker"));
        assert_eq!(registry.factory_count(), 1);

        let bytes = 99u64.to_be_bytes();
        let any = registry.create_actor("test::Worker", &bytes).unwrap();
        let val = any.downcast::<u64>().unwrap();
        assert_eq!(*val, 99);
    }

    #[test]
    fn create_actor_unknown_type_returns_error() {
        let registry = TypeRegistry::new();
        let result = registry.create_actor("unknown::Actor", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("no actor factory"));
    }

    #[cfg(feature = "serde")]
    mod serde_tests {
        use super::*;

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Increment {
            amount: u64,
        }

        #[test]
        fn register_type_and_roundtrip() {
            let mut registry = TypeRegistry::new();
            registry.register_type::<Increment>();

            let msg = Increment { amount: 42 };
            let bytes = serde_json::to_vec(&msg).unwrap();

            let type_name = std::any::type_name::<Increment>();
            assert!(registry.has_type(type_name));

            let any = registry.deserialize(type_name, &bytes).unwrap();
            let deserialized = any.downcast::<Increment>().unwrap();
            assert_eq!(*deserialized, msg);
        }

        #[test]
        fn register_type_invalid_bytes() {
            let mut registry = TypeRegistry::new();
            registry.register_type::<Increment>();

            let type_name = std::any::type_name::<Increment>();
            let result = registry.deserialize(type_name, b"not json");
            assert!(result.is_err());
        }
    }
}
