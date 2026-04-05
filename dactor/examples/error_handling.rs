//! ActorError with ErrorCode and error chains.
//!
//! Run with: cargo run --example error_handling --features test-support

use async_trait::async_trait;
use dactor::actor::{Actor, ActorContext, ActorError, ActorRef, Handler};
use dactor::errors::{ErrorCode, RuntimeError};
use dactor::message::Message;
use dactor::TestRuntime;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

struct Validate(String);
impl Message for Validate {
    type Reply = Result<String, ActorError>;
}

struct Lookup(u64);
impl Message for Lookup {
    type Reply = Result<String, ActorError>;
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

struct Validator;

impl Actor for Validator {
    type Args = ();
    type Deps = ();
    fn create(_: (), _: ()) -> Self {
        Validator
    }
}

#[async_trait]
impl Handler<Validate> for Validator {
    async fn handle(
        &mut self,
        msg: Validate,
        _ctx: &mut ActorContext,
    ) -> Result<String, ActorError> {
        if msg.0.is_empty() {
            return Err(
                ActorError::new(ErrorCode::InvalidArgument, "input must not be empty")
                    .with_details("field: name"),
            );
        }
        if msg.0.len() > 50 {
            let parse_err = ActorError::new(ErrorCode::Internal, "buffer overflow in parser");
            return Err(
                ActorError::new(ErrorCode::InvalidArgument, "input too long").with_cause(parse_err),
            );
        }
        Ok(format!("ok: {}", msg.0))
    }
}

#[async_trait]
impl Handler<Lookup> for Validator {
    async fn handle(&mut self, msg: Lookup, _ctx: &mut ActorContext) -> Result<String, ActorError> {
        match msg.0 {
            1 => Ok("Alice".into()),
            2 => Ok("Bob".into()),
            _ => Err(ActorError::new(
                ErrorCode::NotFound,
                format!("user {} not found", msg.0),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    println!("=== Error Handling Example ===\n");

    let runtime = TestRuntime::new();
    let actor = runtime.spawn::<Validator>("validator", ()).await.unwrap();

    // --- Success case ---
    println!("--- Valid input ---");
    let result = actor
        .ask(Validate("hello".into()), None)
        .unwrap()
        .await
        .unwrap();
    println!("  result: {:?}", result);
    assert!(result.is_ok());

    // --- InvalidArgument ---
    println!("\n--- Empty input (InvalidArgument) ---");
    let result = actor.ask(Validate("".into()), None).unwrap().await.unwrap();
    match &result {
        Err(e) => {
            println!("  code:    {:?}", e.code);
            println!("  message: {}", e.message);
            println!("  details: {:?}", e.details);
            assert_eq!(e.code, ErrorCode::InvalidArgument);
            assert!(e.details.as_deref() == Some("field: name"));
        }
        Ok(_) => panic!("expected error"),
    }

    // --- Error chaining ---
    println!("\n--- Long input (error chain) ---");
    let long_input = "x".repeat(51);
    let result = actor
        .ask(Validate(long_input), None)
        .unwrap()
        .await
        .unwrap();
    match &result {
        Err(e) => {
            println!("  code:    {:?}", e.code);
            println!("  message: {}", e.message);
            println!("  cause:   {:?}", e.cause.as_ref().map(|c| &c.message));
            assert!(e.cause.is_some());
            assert_eq!(e.cause.as_ref().unwrap().code, ErrorCode::Internal);
        }
        Ok(_) => panic!("expected error"),
    }

    // --- NotFound ---
    println!("\n--- Lookup not found ---");
    let result = actor.ask(Lookup(99), None).unwrap().await.unwrap();
    match &result {
        Err(e) => {
            println!("  code:    {:?}", e.code);
            println!("  message: {}", e.message);
            assert_eq!(e.code, ErrorCode::NotFound);
        }
        Ok(_) => panic!("expected error"),
    }

    // --- Demonstrate RuntimeError matching ---
    println!("\n--- RuntimeError from ask() ---");
    let result: Result<Result<String, ActorError>, RuntimeError> =
        actor.ask(Lookup(1), None).unwrap().await;
    match result {
        Ok(inner) => {
            println!("  ask() succeeded, inner result: {:?}", inner);
            assert!(inner.is_ok());
        }
        Err(RuntimeError::Actor(e)) => println!("  actor error: {}", e),
        Err(RuntimeError::Cancelled) => println!("  cancelled"),
        Err(other) => println!("  other runtime error: {}", other),
    }

    println!("\n  ✓ all error scenarios demonstrated");
    println!("\n=== Done ===");
}
