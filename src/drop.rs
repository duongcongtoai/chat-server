use futures_core::task::{Context, Poll};
use futures_util::StreamExt;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{mpsc, oneshot};

pub struct DropReceiver<T> {
    chan: UnboundedSender<usize>,
    inner: mpsc::Receiver<T>,
}
impl<T> DropReceiver<T> {
    pub fn new(inner: mpsc::Receiver<T>) -> (Self, UnboundedReceiver<usize>) {
        let (mut oneshot_tx, oneshot_rx) = unbounded_channel();
        (
            Self {
                chan: oneshot_tx,
                inner,
            },
            oneshot_rx,
        )
    }
}

impl<T> futures_core::stream::Stream for DropReceiver<T> {
    type Item = T;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        Pin::new(&mut self.inner).poll_recv(cx)
    }
}

/* impl<T> Deref for DropReceiver<T> {
    type Target = mpsc::Receiver<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
} */

impl<T> Drop for DropReceiver<T> {
    fn drop(&mut self) {
        println!("Receiver has been dropped");
        self.chan.send(1);
    }
}

impl<T> DerefMut for DropReceiver<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<T> Deref for DropReceiver<T> {
    type Target = mpsc::Receiver<T>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
