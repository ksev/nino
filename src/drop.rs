use anyhow::Result;

#[derive(Debug)]
pub struct DropJoin<T> {
    handle: Option<std::thread::JoinHandle<Result<T>>>,
}

impl<T> DropJoin<T> {
    pub fn new(handle: std::thread::JoinHandle<Result<T>>) -> DropJoin<T> {
        DropJoin {
            handle: Some(handle),
        }
    }
}

impl<T> Drop for DropJoin<T> {
    fn drop(&mut self) {
        if let Some(inner) = self.handle.take() {
            let res = inner
                .join()
                .map_err(|e| anyhow::format_err!("{:?}", e))
                .and_then(|r| r);
            if res.is_err() && !std::thread::panicking() {
                res.unwrap();
            }
        }
    }
}
