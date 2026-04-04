use dactor::message::Message;
use dactor::prelude::*;
use dactor::test_support::test_runtime::TestRuntime;

struct Counter {
    count: u64,
}

impl Actor for Counter {
    type Args = Self;
    type Deps = ();
    fn create(args: Self, _deps: ()) -> Self {
        args
    }
}

struct Increment(u64);
impl Message for Increment {
    type Reply = ();
}

struct GetCount;
impl Message for GetCount {
    type Reply = u64;
}

#[async_trait::async_trait]
impl Handler<Increment> for Counter {
    async fn handle(&mut self, msg: Increment, _ctx: &mut ActorContext) {
        self.count += msg.0;
    }
}

#[async_trait::async_trait]
impl Handler<GetCount> for Counter {
    async fn handle(&mut self, _msg: GetCount, _ctx: &mut ActorContext) -> u64 {
        self.count
    }
}

#[tokio::main]
async fn main() {
    let runtime = TestRuntime::new();
    let counter = runtime.spawn::<Counter>("counter", Counter { count: 0 });

    counter.tell(Increment(5)).unwrap();
    counter.tell(Increment(3)).unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let count = counter.ask(GetCount, None).unwrap().await.unwrap();
    println!("Count: {count}"); // Count: 8
}
