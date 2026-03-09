//! # A Whorlwind Tour in Building a Rust Async Executor
//!
//! whorl is a self contained library to run asynchronous Rust code with the
//! following goals in mind:
//!
//! - Keep it in one file. You should be able to read this code beginning to end
//!   like a literate program and understand what each part does and how it fits
//!   into the larger narrative. The code is organized to tell a story, not
//!   necessarily how I would normally structure Rust code.
//! - Teach others what is going on when you run async code in Rust with a runtime
//!   like tokio. There is no magic, just many synchronous functions in an async
//!   trenchcoat.
//! - Explain why different runtimes are incompatible, even if they all run async
//!   programs.
//! - Only use the `std` crate to show that yes all the tools to build one exist
//!   and if you wanted to, you could. Note we only use the rand crate for our
//!   example test, it is not used for the runtime itself
//! - Use only stable Rust. You can build this today; no fancy features needed.
//! - Explain why `std` doesn't ship an executor, but just the building blocks.
//!
//! What whorl isn't:
//! - Performant, this is an adaptation of a class I gave at Rustconf back in
//!   2018. Its first and foremost goal is to teach *how* an executor
//!   works, not the best way to make it fast. Reading the tokio source
//!   code would be a really good thing if you want to learn about how to make
//!   things performant and scalable.
//! - "The Best Way". Programmers have opinions, I think we should maybe have
//!   less of them sometimes. Even me. You might disagree with an API design
//!   choice or a way I did something here and that's fine. I just want you to
//!   learn how it all works. Async executors also have trade offs so one way
//!   might work well for your code and not for another person's code.
//! - An introduction to Rust. This assumes you're somewhat familiar with it and
//!   while I've done my best to break it down so that it is easy to understand,
//!   that just might not be the case and I might gloss over details given I've
//!   used Rust since 1.0 came out in 2015. Expert blinders are real and if
//!   things are confusing, do let me know in the issue tracker. I'll try my best
//!   to make it easier to grok, but if you've never touched Rust before, this is
//!   in all honesty not the best place to start.
//!
//! With all of that in mind, let's dig into it all!

pub mod futures {
    //! This is our module to provide certain kinds of futures to users. In the case
    //! of our [`Sleep`] future here, this is not dependent on the runtime in
    //! particular. We would be able to run this on any executor that knows how to
    //! run a future. Where incompatibilities arise is if you use futures or types
    //! that depend on the runtime or traits not defined inside of the standard
    //! library. For instance, `std` does not provide an `AsyncRead`/`AsyncWrite`
    //! trait as of Oct 2024. As a result, if you want to provide the functionality
    //! to asynchronously read or write to something, then that trait tends to be
    //! written for an executor. So tokio would have its own `AsyncRead` and so
    //! would ours for instance. Now if a new library wanted to write a type that
    //! can, say, read from a network socket asynchronously, they'd have to write an
    //! implementation of `AsyncRead` for both executors. Not great. Another way
    //! incompatibilities can arise is when those futures depend on the state of the
    //! runtime itself. Now that implementation is locked to the runtime.
    //!
    //! Sometimes this is actually okay; maybe the only way to implement
    //! something is depending on the runtime state. In other ways it's not
    //! great. Things like `AsyncRead`/`AsyncWrite` would be perfect additions
    //! to the standard library at some point since they describe things that
    //! everyone would need, much like how `Read`/`Write` are in stdlib and we
    //! all can write generic code that says, "I will work with anything that I
    //! can read or write to."
    //!
    //! This is why, however, things like Future, Context, Wake, Waker etc. all
    //! the components we need to build an executor are in the standard library.
    //! It means anyone can build an executor and accept most futures or work
    //! with most libraries without needing to worry about which executor they
    //! use. It reduces the burden on maintainers and users. In some cases
    //! though, we can't avoid it. Something to keep in mind as you navigate the
    //! async ecosystem and see that some libraries can work on any executor or
    //! some ask you to opt into which executor you want with a feature flag.
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
        time::SystemTime,
    };

    /// A future that will allow us to sleep and block further execution of the
    /// future it's used in without blocking the thread itself. It will be
    /// polled and if the timer is not up, then it will yield execution to the
    /// executor.
    pub struct Sleep {
        /// What time the future was created at, not when it was started to be
        /// polled.
        now: SystemTime,
        /// How long in the future in ms we must wait till we return
        /// that the future has finished polling.
        ms: u128,
    }

    impl Sleep {
        /// A simple API whereby we take in how long the consumer of the API
        /// wants to sleep in ms and set now to the time of creation and
        /// return the type itself, which is a Future.
        pub fn new(ms: u128) -> Self {
            Self {
                now: SystemTime::now(),
                ms,
            }
        }
    }

    impl Future for Sleep {
        /// We don't need to return a value for [`Sleep`], as we just want it to
        /// block execution for a while when someone calls `await` on it.
        type Output = ();
        /// The actual implementation of the future, where you can call poll on
        /// [`Sleep`] if it's pinned and the pin has a mutable reference to
        /// [`Sleep`]. In this case we don't need to utilize
        /// [`Context`][std::task::Context] here and in fact you often will not.
        /// It only serves to provide access to a `Waker` in case you need to
        /// wake the task. Since we always do that in our executor, we don't need
        /// to do so here, but you might find if you manually write a future
        /// that you need access to the waker to wake up the task in a special
        /// way. Waking up the task just means we put it back into the executor
        /// to be polled again.
        fn poll(self: Pin<&mut Self>, _: &mut Context) -> Poll<Self::Output> {
            // If enough time has passed, then when we're polled we say that
            // we're ready and the future has slept enough. If not, we just say
            // that we're pending and need to be re-polled, because not enough
            // time has passed.
            if self.now.elapsed().unwrap().as_millis() >= self.ms {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }

    // In practice, what we do when we sleep is something like this:
    // ```
    // async fn example() {
    //     Sleep::new(2000).await;
    // }
    // ```
    //
    // Which is neat and all but how is that future being polled? Well, this
    // all desugars out to:
    // ```
    // async fn example() {
    //     let mut sleep = Sleep::new(2000);
    //     loop {
    //        match Pin::new(sleep).as_mut().poll(&mut context) {
    //            Poll::Ready(()) => (),
    //            // You can't write yield yourself as this is an unstable
    //            // feature currently
    //            Poll::Pending => yield,
    //        }
    //     }
    // }
}

#[test]
/// To understand what we'll build, we need to see and understand what we will
/// run and the output we expect to see. Note that if you wish to run this test,
/// you should use the command `cargo test -- --nocapture` so that you can see
/// the output of `println` being used, otherwise it'll look like nothing is
/// happening at all for a while.
fn library_test() {
    // We're going to import our Sleep future to make sure that it works,
    // because it's not a complicated future and it's easy to see the
    // asynchronous nature of the code.
    use crate::{futures::Sleep, runtime};
    // We want some random numbers so that the sleep futures finish at different
    // times. If we didn't, then the code would look synchronous in nature even
    // if it isn't. This is because we schedule and poll tasks in what is
    // essentially a loop unless we use block_on.
    use rand::Rng;
    // We need to know the time to show when a future completes. Time is cursed
    // and it's best we do not dabble too much in it.
    use std::time::SystemTime;

    // This function causes the runtime to block on this future. It does so by
    // just taking this future and polling it till completion in a loop and
    // ignoring other tasks on the queue. Sometimes you need to block on async
    // functions and treat them as sync. A good example is running a webserver.
    // You'd want it to always be running, not just sometimes, and so blocking
    // it makes sense. In a single threaded executor this would block all
    // execution. In our case our executor is single-threaded. Technically it
    // runs on a separate thread from our program and so blocks running other
    // tasks, but the main function will keep running. This is why we call
    // `wait` to make sure we wait till all futures finish executing before
    // exiting.
    runtime::block_on(async {
        const SECOND: u128 = 1000; //ms
        println!("Begin Asynchronous Execution");
        // Create a random number generator so we can generate random numbers
        let mut rng = rand::thread_rng();

        // A small function to generate the time in seconds when we call it.
        let time = || {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };

        // Spawn 5 different futures on our executor
        for i in 0..5 {
            // Generate the two numbers between 1 and 9. We'll spawn two futures
            // that will sleep for as many seconds as the random number creates
            let random = rng.gen_range(1..10);
            let random2 = rng.gen_range(1..10);

            // We now spawn a future onto the runtime from within our future
            runtime::spawn(async move {
                println!("Spawned Fn #{:02}: Start {}", i, time());
                // This future will sleep for a certain amount of time before
                // continuing execution
                Sleep::new(SECOND * random).await;
                // After the future waits for a while, it then spawns another
                // future before printing that it finished. This spawned future
                // then sleeps for a while and then prints out when it's done.
                // Since we're spawning futures inside futures, the order of
                // execution can change.
                runtime::spawn(async move {
                    Sleep::new(SECOND * random2).await;
                    println!("Spawned Fn #{:02}: Inner {}", i, time());
                });
                println!("Spawned Fn #{:02}: Ended {}", i, time());
            });
        }
        // To demonstrate that block_on works we block inside this future before
        // we even begin polling the other futures.
        runtime::block_on(async {
            // This sleeps longer than any of the spawned functions, but we poll
            // this to completion first even if we await here.
            Sleep::new(11000).await;
            println!("Blocking Function Polled To Completion");
        });
    });

    // We now wait on the runtime to complete each of the tasks that were
    // spawned before we exit the program
    runtime::wait();
    println!("End of Asynchronous Execution");

    // When all is said and done when we run this test we should get output that
    // looks somewhat like this (though in different orders in each execution):
    //
    // Begin Asynchronous Execution
    // Blocking Function Polled To Completion
    // Spawned Fn #00: Start 1634664688
    // Spawned Fn #01: Start 1634664688
    // Spawned Fn #02: Start 1634664688
    // Spawned Fn #03: Start 1634664688
    // Spawned Fn #04: Start 1634664688
    // Spawned Fn #01: Ended 1634664690
    // Spawned Fn #01: Inner 1634664691
    // Spawned Fn #04: Ended 1634664694
    // Spawned Fn #04: Inner 1634664695
    // Spawned Fn #00: Ended 1634664697
    // Spawned Fn #02: Ended 1634664697
    // Spawned Fn #03: Ended 1634664697
    // Spawned Fn #00: Inner 1634664698
    // Spawned Fn #03: Inner 1634664698
    // Spawned Fn #02: Inner 1634664702
    // End of Asynchronous Execution
}

pub mod runtime {
    use std::{
        // Including this lets us use a handy little directive to automatically make
        // types `Clone`. That is, it lets us make deep copies of types that we may
        // or may not be able to make shallow copies of.
        clone::Clone,
        // We need a place to put the futures that get spawned onto the runtime
        // somewhere and while we could use something like a `Vec`, we chose a
        // `LinkedList` here. One reason being that we can put tasks at the front of
        // the queue if they're a blocking future. The other being that we use a
        // constant amount of memory. We only ever use as much as we need for tasks.
        // While this might not matter at a small scale, this does at a larger
        // scale. If your `Vec` never gets smaller and you have a huge burst of
        // tasks under, say, heavy HTTP loads in a web server, then you end up eating
        // up a lot of memory that could be used for other things running on the
        // same machine. In essence what you've created is a kind of memory leak
        // unless you make sure to resize the `Vec`. @mycoliza did a good Twitter
        // thread on this here if you want to learn more!
        //
        // https://twitter.com/mycoliza/status/1298399240121544705
        collections::LinkedList,
        // A Future is the fundamental block of any async executor. It is a trait
        // that types can make or an unnameable type that an async function can
        // make. We say it's unnameable because you don't actually define the type
        // anywhere and just like a closure you can only specify its behavior with
        // a trait. You can't give it a name like you would when you do something
        // like `pub struct Foo;`. These types, whether nameable or not, represent all
        // the state needed to have an asynchronous function. You poll the future to
        // drive its computation along like a state machine that makes transistions
        // from one state to another till it finishes. If you reach a point where it
        // would yield execution, then it needs to be rescheduled to be polled again
        // in the future. It yields though so that you can drive other futures
        // forward in their computation!
        //
        // This is the important part to understand here with the executor: the
        // Future trait defines the API we use to drive forward computation of it,
        // while the implementor of the trait defines how that computation will work
        // and when to yield to the executor. You'll see later that we have an
        // example of writing a `Sleep` future by hand as well as unnameable async
        // code using `async { }` and we'll expand on when those yield and what it
        // desugars to in practice. We're here to demystify the mystical magic of
        // async code.
        future::Future,
        // Ah Pin. What a confusing type. The best way to think about `Pin` is that
        // it records when a value became immovable or pinned in place. `Pin` doesn't
        // actually pin the value, it just notes that the value will not move, much
        // in the same way that you can specify Rust lifetimes. It only records what
        // the lifetime already is, it doesn't actually create said lifetime! At the
        // bottom of this, I've linked some more in depth reading on Pin, but if you
        // don't know much about Pin, starting with the standard library docs isn't a
        // bad place.
        //
        // Note: Unpin is also a confusing name and if you think of it as
        // MaybePinned you'll have a better time as the value may be pinned or it
        // may not be pinned. It just marks that if you have a Pinned value and it
        // moves that's okay and it's safe to do so, whereas for types that do not
        // implement Unpin and they somehow move, will cause some really bad things
        // to happen since it's not safe for the type to be moved after being
        // pinned. We create our executor with the assumption that every future we
        // get will need to be a pinned value, even if it is actually Unpin. This
        // makes it nicer for everyone using the executor as it's very easy to make
        // types that do not implement Unpin.
        pin::Pin,
        sync::{
            // What's not to love about Atomics? This lets us have thread safe
            // access to primitives so that we can modify them or load them using
            // Ordering to tell the compiler how it should handle giving out access
            // to the data. Atomics are a rather deep topic that's out of scope for
            // this. Just note that we want to change a usize safely across threads!
            // If you want to learn more Mara Bos' book `Rust Atomics and Locks` is
            // incredibly good:  https://marabos.nl/atomics/
            atomic::{AtomicUsize, Ordering},
            // Arc is probably one of the more important types we'll use in the
            // executor. It lets us freely clone cheap references to the data which
            // we can use across threads while making it easy to not have to worry about
            // complicated lifetimes since we can easily own the data with a call to
            // clone. It's one of my favorite types in the standard library.
            Arc,
            // We use (zero-buffer) synchronous channels to pass futures to the executor
            // thread. This lets us transmit futures between threads, and syncrhonize
            // those threads at the point of transmission. Unlike in some other languages
            // (e.g. Go), Rust's channels are strictly single-consumer. Hence why the module
            // is named MPSC: Multiple Producer, Single Consumer. This is fine for what we're
            // doing as we've only got a single thread resposible for creating tasks. That's
            // a limitation of using tracing::subscriber::set_default to perform tracing in
            // the executor (as opposed to tracing::subscriber::set_global_default, or a
            // more complex multi-threaded arrangement).
            mpsc::{SyncSender, Receiver, sync_channel},
            // Normally I would use `parking_lot` for a Mutex, but the goal is to
            // use stdlib only. A personal gripe is that it cares about Mutex
            // poisoning (when a thread panics with a hold on the lock), which is
            // not something I've in practice run into (others might!) and so calling
            // `lock().unwrap()` everywhere can get a bit tedious. That being said
            // Mutexes are great. You make sure only one thing has access to the data
            // at any given time to access or change it.
            Mutex,
        },
        // The task module contains all of the types and traits related to
        // having an executor that can create and run tasks that are `Futures`
        // that need to be polled.
        task::{
            // `Context` is passed in every call to `poll` for a `Future`. We
            // didn't use it in our `Sleep` one, but it has to be passed in
            // regardless. It gives us access to the `Waker` for the future so
            // that we can call it ourselves inside the future if need be!
            Context,
            // Poll is the enum returned from when we poll a `Future`. When we
            // call `poll`, this drives the `Future` forward until it either
            // yields or it returns a value. `Poll` represents that. It is
            // either `Poll::Pending` or `Poll::Ready(T)`. We use this to
            // determine if a `Future` is done or not and if not, then we should
            // keep polling it.
            Poll,
            // This is a trait to define how something in an executor is woken
            // up. We implement it for `Task` which is what lets us create a
            // `Waker` from it, to then make a `Context` which can then be
            // passed into the call to `poll` on the `Future` inside the `Task`.
            Wake,
            // A `Waker` is the type that has a handle to the runtime to let it
            // know when a task is ready to be scheduled for polling. We're
            // doing a very simple version where as soon as a `Task` is done
            // polling we tell the executor to wake it. Instead what you might
            // want to do when creating a `Future` is have a more involved way
            // to only wake when it would be ready to poll, such as a timer
            // completing, or listening for some kind of signal from the OS.
            // It's kind of up to the executor how it wants to do it. Maybe how
            // it schedules things is different or it has special behavior for
            // certain `Future`s that it ships with it. The key thing to note
            // here is that this is how tasks are supposed to be rescheduled for
            // polling.
            Waker,
        },
    };

    // TODO
    use tracing::{Level, span::EnteredSpan, instrument::Instrument};
    use tracing_subscriber::fmt::format::FmtSpan;

    /// This is what actually drives all of our async code. We spawn a separate
    /// executor thread that loops, popping tasks off of a queue and polling them.
    /// After the executor is launched, an interface is returned which allows users
    /// in other threads to communicate with the executor spawned here. Every call
    /// to this function creates a new executor with its own runtime and interface.
    pub fn start() -> Interface {
        // Before spinning off a separate thread for the executor, we need to create
        // some shared state and synchronization primitives.
        let mut runtime = Runtime::new();
        let (future_tx, future_rx) = sync_channel(0);

        // Here we take some of the objects constructed above and bundle them together
        // to build this executor's interface. Some of these (e.g. future_tx) will
        // be used externally, others will be used internally (e.g. queue), while
        // others still (e.g. tasks) will be used in both places.
        let interface = Interface {
            queue: runtime.queue.clone(),
            tasks: Arc::new(AtomicUsize::new(0)),
            future_tx,
        };

        // We'll also need to make a copy of the interface for external use.
        let external_interface = interface.clone();

        // Then we spin off the executor thread!
        std::thread::spawn(move || {
            // Before we start the executor's event loop, we set up a tracing::Subscriber
            // so it can produce logs that we can interpret! This subscriber applies only
            // to this thread.
            let _guard = tracing::subscriber::set_default(
                tracing_subscriber::fmt::fmt()
                    .with_span_events(FmtSpan::ACTIVE)
                    .with_level(true)
                    .with_max_level(Level::TRACE)
                    .with_writer(std::io::stdout)
                    .finish()
            );

            loop {
                // This is the start of the executor's event loop. We begin by looking for new
                // futures to add to the queue as Tasks.
                runtime.queue_all(&interface, &future_rx);

                // Next we try to pop a Task from the queue. This must be done before matching
                // on the Task, otherwise we wouldn't release the lock!
                let task = runtime.queue.lock().unwrap().dequeue();

                match task {
                    None => continue,
                    Some(task) => {
                        // If the queue had a Task on it, we poll() it. If the Task is blocking,
                        // it is polled until completion. While a blocking Task is being polled we
                        // keep looking for new Tasks coming in. If the Task is non-blocking, we put
                        // it back into the queue if it's still pending. Otherwise, Tasks that are ready
                        // are simply dropped from the queue as they're now finished running.
                        if task.will_block() {
                            while task.poll().is_pending() {
                                runtime.queue_all(&interface, &future_rx);
                            }
                        } else {
                            if task.poll().is_pending() {
                                task.wake();
                            }
                        }
                    },
                };
            }
        });

        external_interface
    }

    /// This is it, the thing we've been alluding to for most of this file. It's
    /// the `Runtime`! What is it? What does it do? Well the `Runtime` is what
    /// retains the state that helps the executor drive our async code to completion.
    /// Remember asynchronous code is just code that gets run for a bit, yields part
    /// way through the function, then continues when polled and it repeats this process
    /// till being completed. In reality what this means is that the code is run
    /// using synchronous functions that drive tasks in a concurrent manner.
    /// They could also be run concurrently and/or in parallel if the executor
    /// is multithreaded. Tokio is a good example of this model where it runs
    /// tasks in parallel on separate threads and if it has more tasks than
    /// threads, it runs them concurrently on those threads.
    ///
    /// Our `Runtime` in particular has:
    struct Runtime {
        /// A queue to place all of the tasks that are spawned on the runtime.
        /// The queue is wrapped in an `Arc<Mutex<...>>` to enable an access
        /// pattern called "interior mutability". This permits more fine-grained
        /// access to queue references (mutable and immutable). This is important
        /// for implementing `Wake` functionality on the `Task` type, because a
        /// task must retain a reference to this queue to `requeue()` itself.
        /// We can't just go around handing out mutable references to the queue,
        /// as that would violate rules about exclusive ownership of mutable
        /// references! Implementing `Wake` also requires that `Task` be `Sync`
        /// (and that the reference to the queue it holds is also `Sync`), so we
        /// use a `Mutex` rather than a `RefCell` even though the executor
        /// is single-threaded. For more details on "interior mutability" see:
        /// https://doc.rust-lang.org/book/ch15-05-interior-mutability.html
        queue: Arc<Mutex<Queue>>,
        /// A counter which tracks the total number of tasks spawned. We use
        /// this to assign tags to tasks for tracking purposes.
        tags: usize,
    }

    /// Our `Runtime` type is designed such that we can have several running
    /// simultaneously. In production code you might want multiple runtimes so
    /// you can limit what happens on one for a free tier version and let the
    /// non-free version use as many resources as it can.
    /// 
    /// Our runtime implements 3 functions: `new` to create a new `Runtime`,
    /// `queue_new` to spawn a new task on the runtime, and `queue_all` to
    /// spawn new tasks on the runtime for every future recieved through a
    /// `Reciever` channel.
    impl Runtime {
        /// This creates a new Runtime.
        fn new() -> Self {
            Runtime {
                queue: Arc::new(Mutex::new(Queue::new())),
                tags: 0,
            }
        }

        /// We use this to add a new task to the queue, incrementing the tag counter
        /// at the same time.
        fn queue_new(self: &mut Self, interface: &Interface, block: Blocking, future: impl Future<Output = ()> + Send + Sync + 'static) {
            self.queue.lock().unwrap().requeue(Task::new(interface.clone(), self.tags, block, future));
            self.tags += 1;
        }

        /// Here we read as many futures as we can from a `Receiver`. All futures read
        /// from the channel are then added to the queue as new tasks.
        fn queue_all(self: &mut Self, interface: &Interface, future_rx: &Receiver<TxRxFuture>) {
            while let Ok((block, future)) = future_rx.try_recv() {
                self.queue_new(interface, block, future);
            }
        }
    }

    /// The `Interface` provides the means for users in other threads to
    /// interact with an executor. The `Task` objects internal to the
    /// runtime also interact with higher-level data types (like the queue)
    /// using the access/synchronization structures in this type.
    #[derive(Clone)]
    pub struct Interface {
        /// A shareable reference to a `Queue`. As noted elsewhere, the `Mutex`
        /// helps enable "interior mutability".
        queue: Arc<Mutex<Queue>>,
        /// A counter for how many tasks are currently on the runtime. We use
        /// this in conjunction with `wait()` to block until there are no more
        /// tasks on the runtime.
        tasks: Arc<AtomicUsize>,
        /// A channel along which users can send futures to the executor to
        /// spawn new tasks.
        future_tx: SyncSender<TxRxFuture>,
    }

    /// The Interface allows users of its runtime to perform a few different
    /// functions. Users can `spawn` new non-blocking tasks, `block_on` new
    /// blocking tasks and `wait` until all tasks on the runtime are complete.
    impl Interface {
        /// Spawn a non-blocking future onto the associated runtime.
        pub fn spawn(self: &Self, future: impl Future<Output = ()> + Send + Sync + 'static) {
            if let Ok(_) = self.future_tx.send((Blocking::NonBlocking, Box::pin(future))) {
                self.tasks.fetch_add(1, Ordering::AcqRel);
            }
        }

        /// Block on a future and stop others on the associated runtime until this
        /// one completes.
        pub fn block_on(self: &Self, future: impl Future<Output = ()> + Send + Sync + 'static) {
            if let Ok(_) = self.future_tx.send((Blocking::Blocking, Box::pin(future))) {
                self.tasks.fetch_add(1, Ordering::AcqRel);
            }
        }

        /// Block further execution of a program until all of the tasks on the associated
        /// runtime are completed.
        pub fn wait(self: &Self) {
            // This operation is "Acquire" so that previous "AcqRel" fetch_add operations
            // executed in the same thread are always visible to it. Essentially, we're
            // ensuring that the compiler will never re-order fetch_add operations (from
            // the same thread) before this load operation.
            while self.tasks.load(Ordering::Acquire) > 0 {}
        }
    }

    /// This is an alias for a pair composed of (1) a blocking/non-blocking directive and
    /// (2) an associated future. This is used for transmission of task initialization
    /// information through the channel held by an interface to its executor.
    type TxRxFuture = (Blocking, Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>);

    /// The `Queue` type encapsulates a singly-linked list that contains all of the tasks
    /// being run on it. This type keeps no information about the `tracing::Span` objects
    /// associated with live tasks, as it is meant to be `Sync` (shareable between threads).
    /// This isn't the most efficient pattern especially if we wanted to have the runtime
    /// be truly multi-threaded, but for the purposes of the code this works just fine.
    struct Queue {
        list: LinkedList<Arc<Task>>,
    }

    impl Queue {
        /// Creates a new `Queue` object.
        fn new() -> Self {
            Queue {
                list: LinkedList::new(),
            }
        }

        /// This function removes a task from the front of the queue and returns it
        /// if the queue is non-empty. Otherwise it returns `None`.
        fn dequeue(self: &mut Self) -> Option<Arc<Task>> {
            self.list.pop_front()
        }

        /// This function just takes a task and pushes it onto the queue. We use this
        /// both for newly spawned tasks and to push old ones that get woken up back
        /// onto the queue. If the task blocks, it is added to the front of the queue,
        /// otherwise it is added to the back.
        fn requeue(self: &mut Self, task: Arc<Task>) {
            if task.will_block() {
                self.list.push_front(task);
            } else {
                self.list.push_back(task);
            }
        }
    }

    /// An enum used to assert whether or not a task or future will block.
    enum Blocking {
        NonBlocking,
        Blocking,
    }

    /// The `Task` is the basic unit for the executor. It represents a `Future`
    /// that may or may not be completed. We spawn `Task`s to be run and poll
    /// them until completion in a non-blocking manner unless specifically asked
    /// for.
    struct Task {
        /// This is the actual `Future` we will poll inside of a `Task`. We `Box`
        /// and `Pin` the `Future` when we create a task so that we don't need
        /// to worry about pinning or more complicated things in the runtime. We
        /// also need to make sure this is `Send + Sync` so we can use it across threads
        /// and so we lock the `Pin<Box<dyn Future>>` inside a `Mutex`.
        /// It's worth noting that boxing a `Future` is `Pin` since the `Box` is
        /// a pointer to an item on the heap. The pointer can be moved around, but
        /// what it points too won't move at all until it is dropped. When we call
        /// `Box::pin` we're just putting the `Future` on the heap and marking for
        /// the type system that this type will not move anymore.
        future: Mutex<Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>>,
        /// We need a way to check if the runtime should block on this task and
        /// so we use an enum here to check that!
        block: Blocking,
        /// We need to know some information about the runtime on which this task
        /// is being run, so we keep a copy of the runtime's interface.
        interface: Interface,
        /// We use this to help identify Tasks for tracking/debugging purposes.
        tag: usize,
    }

    impl Task {
        /// This constructs a new task.
        fn new(interface: Interface, tag: usize, block: Blocking, future: impl Future<Output = ()> + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Task {
                future: Mutex::new(Box::pin(future.instrument(tracing::span!(Level::TRACE, "Future", Task=tag)))),
                block,
                interface,
                tag,
            })
        }

        /// We want to use the `Task` itself as a `Waker` which we'll get more
        /// into below. This is a convenience method to construct a new `Waker`.
        /// A neat thing to note for `poll` and here as well is that we can
        /// restrict a method such that it will only work when `self` is a
        /// certain type. In this case you can only call `waker` if the type is
        /// a `&Arc<Task>`. If it was just `Task` it would not compile or work.
        fn waker(self: &Arc<Self>) -> Waker {
            self.clone().into()
        }

        /// This is a convenience method to `poll` a `Future` by creating the
        /// `Waker` and `Context` and then getting access to the actual `Future`
        /// inside the `Mutex` and calling `poll` on that.
        fn poll(self: &Arc<Self>) -> Poll<()> {
            let waker = self.waker();
            let mut ctx = Context::from_waker(&waker);
            self.future.lock().unwrap().as_mut().poll(&mut ctx)
        }

        /// Checks the `block` field to see if the `Task` is blocking.
        fn will_block(self: &Self) -> bool {
            match self.block {
                Blocking::NonBlocking => false,
                Blocking::Blocking => true,
            }
        }
    }

    /// Since we increase the count everytime we send a new future to the
    /// executor, we need to make sure that we *also* decrease the count every
    /// time a task goes out of scope. This implementation of `Drop` does
    /// just that so that we don't need to bookkeep about when and where to
    /// subtract from the count.
    impl Drop for Task {
        fn drop(self: &mut Self) {
            self.interface.tasks.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// `Wake` is the crux of all of this executor as it's what lets us
    /// reschedule a task when it's ready to be polled. For our implementation
    /// we push it back onto the runtime's queue according to whether or not it blocks.
    impl Wake for Task {
        fn wake(self: Arc<Self>) {
            self.interface.queue.lock().unwrap().requeue(self.clone());
        }
    }
}

// That's it! A full asynchronous runtime with comments all in less than 1000
// lines. Most of that being the actual comments themselves. I hope this made
// how Rust async executors work less magical and more understandable. It's a
// lot to take in, but at the end of the day it's just keeping track of state
// and a couple of loops to get it all working. If you want to see how to write
// a more performant executor that's being used in production and works really
// well, then consider reading the source code for `tokio`. I myself learned
// quite a bit reading it and it's fascinating and fairly well documented.
// If you're interested in learning even more about async Rust or you want to
// learn more in-depth things about it, then I recommend reading this list
// of resources and articles I've found useful that are worth your time:
//
// - Asynchronous Programming in Rust: https://rust-lang.github.io/async-book/01_getting_started/01_chapter.html
// - Getting in and out of trouble with Rust futures: https://fasterthanli.me/articles/getting-in-and-out-of-trouble-with-rust-futures
// - Pin and Suffering: https://fasterthanli.me/articles/pin-and-suffering
// - Understanding Rust futures by going way too deep: https://fasterthanli.me/articles/understanding-rust-futures-by-going-way-too-deep
// - How Rust optimizes async/await
//   - Part 1: https://tmandry.gitlab.io/blog/posts/optimizing-await-1/
//   - Part 2: https://tmandry.gitlab.io/blog/posts/optimizing-await-2/
// - The standard library docs have even more information and are worth reading.
//   Below are the modules that contain all the types and traits necessary to
//   actually create and run async code. They're fairly in-depth and sometimes
//   require reading other parts to understand a specific part in a really weird
//   dependency graph of sorts, but armed with the knowledge of this executor it
//   should be a bit easier to grok what it all means!
//   - task module: https://doc.rust-lang.org/stable/std/task/index.html
//   - pin module: https://doc.rust-lang.org/stable/std/pin/index.html
//   - future module: https://doc.rust-lang.org/stable/std/future/index.html
