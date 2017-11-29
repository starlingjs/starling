use futures::{Async, Future};
use std::fmt::Debug;
use std::marker::PhantomData;

#[derive(Debug, Clone)]
pub(crate) struct OrDefault<F, E>(F, PhantomData<E>);

impl<F, E> Future for OrDefault<F, E>
where
    F: Future,
    F::Item: Default,
{
    type Item = F::Item;
    type Error = E;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        match self.0.poll() {
            Ok(x) => Ok(x),
            Err(_) => Ok(Async::Ready(Self::Item::default())),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IgnoreResults<F, E>(F, PhantomData<E>);

impl<F, E> Future for IgnoreResults<F, E>
where
    F: Future,
{
    type Item = ();
    type Error = E;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        match self.0.poll() {
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Ok(Async::Ready(_)) | Err(_) => Ok(Async::Ready(())),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Unwrap<F, E>(F, PhantomData<E>);

impl<F, E> Future for Unwrap<F, E>
where
    F: Future,
    F::Error: Debug,
{
    type Item = F::Item;
    type Error = E;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        Ok(self.0.poll().unwrap())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Expect<'a, F, E>(F, &'a str, PhantomData<E>);

impl<'a, F, E> Future for Expect<'a, F, E>
where
    F: Future,
    F::Error: Debug,
{
    type Item = F::Item;
    type Error = E;

    fn poll(&mut self) -> Result<Async<Self::Item>, Self::Error> {
        Ok(self.0.poll().expect(self.1))
    }
}

/// Extension methods for `Future`.
pub(crate) trait FutureExt: Future {
    /// If the future results in an error, instead return the default
    /// `Self::Item` value.
    fn or_default<E>(self) -> OrDefault<Self, E>
    where
        Self: Sized,
        Self::Item: Default,
    {
        OrDefault(self, PhantomData)
    }

    /// Ignore the results (both `Ok` and `Err`) of this future. Just drive it
    /// to completion.
    fn ignore_results<E>(self) -> IgnoreResults<Self, E>
    where
        Self: Sized,
    {
        IgnoreResults(self, PhantomData)
    }

    /// Panic if this future results in an error. Like `Result::unwrap`.
    fn unwrap<E>(self) -> Unwrap<Self, E>
    where
        Self: Sized,
        Self::Error: Debug,
    {
        Unwrap(self, PhantomData)
    }

    /// Panic with the given message if this future results in an error. Like
    /// `Result::expect`.
    fn expect<'a, E>(self, msg: &'a str) -> Expect<'a, Self, E>
    where
        Self: Sized,
        Self::Error: Debug,
    {
        Expect(self, msg, PhantomData)
    }
}

impl<T: Future> FutureExt for T {}
