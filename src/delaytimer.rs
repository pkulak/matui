use std::{
    cell::Cell,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{runtime::Handle, task::JoinHandle, time};

pub struct DelayTimer {
    waiter: Cell<Option<JoinHandle<()>>>,
    delay: Duration,
    block: Arc<Mutex<dyn Fn() + Send + Sync>>,
}

impl DelayTimer {
    pub fn new(delay: Duration, block: impl Fn() + 'static + Send + Sync) -> Self {
        DelayTimer {
            waiter: Cell::new(Option::None),
            delay,
            block: Arc::new(Mutex::new(block)),
        }
    }

    pub fn record(&self, rt: Handle) {
        let block = self.block.clone();
        let delay = self.delay;

        let handle = rt.spawn(async move {
            time::sleep(delay).await;
            (block.lock().unwrap())();
        });

        if let Some(h) = self.waiter.replace(Some(handle)) {
            h.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn it_runs_the_closure() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .unwrap();

        let h = rt.handle();

        // this is a test, we can leak an int
        let counter: &'static Mutex<u64> = Box::leak(Box::new(Mutex::new(0)));

        let timer = DelayTimer::new(Duration::from_millis(10), move || {
            *counter.lock().unwrap() += 1;
        });

        // send a few updates through
        timer.record(h.clone());
        timer.record(h.clone());
        timer.record(h.clone());

        // wait a bit
        thread::sleep(Duration::from_millis(20));

        // and we should have called only once
        assert_eq!(*counter.lock().unwrap(), 1);

        // send a couple more
        timer.record(h.clone());
        timer.record(h.clone());

        // nothing yet
        assert_eq!(*counter.lock().unwrap(), 1);

        // wait a bit
        thread::sleep(Duration::from_millis(20));

        // and we should have one more
        assert_eq!(*counter.lock().unwrap(), 2);
    }
}
