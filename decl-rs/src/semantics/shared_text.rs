//! Thin immutable text handles with explicit sharing across path boundaries.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;
use std::rc::Rc;

/// Shared UTF-8 text held through a thin descriptor.
///
/// Value clones share the descriptor. Paths can retain the inner `Rc<str>`
/// without copying characters; its strong count does not count each descriptor
/// clone separately. Equality, ordering and hashing compare text, not owners.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SharedText(Rc<Rc<str>>);

impl SharedText {
    /// Retain an existing character allocation in a new shared descriptor.
    pub fn from_rc(text: Rc<str>) -> Self {
        Self(Rc::new(text))
    }

    /// Borrow the character owner without changing either reference count.
    pub fn as_rc(&self) -> &Rc<str> {
        &self.0
    }

    /// Retain the character allocation independently of this descriptor.
    pub fn to_rc(&self) -> Rc<str> {
        self.as_rc().clone()
    }

    /// Move out a unique descriptor's character owner, or retain a shared one.
    pub fn into_rc(self) -> Rc<str> {
        Rc::unwrap_or_clone(self.0)
    }

    /// Mutate text without changing retained descriptors or exported owners.
    /// Both sharing levels use copy-on-write, including weak-only dissociation.
    pub fn make_mut(&mut self) -> &mut str {
        Rc::make_mut(Rc::make_mut(&mut self.0))
    }
}

impl From<Rc<str>> for SharedText {
    fn from(text: Rc<str>) -> Self {
        Self::from_rc(text)
    }
}

impl From<&str> for SharedText {
    fn from(text: &str) -> Self {
        Self::from_rc(Rc::from(text))
    }
}

impl From<String> for SharedText {
    fn from(text: String) -> Self {
        Self::from_rc(Rc::from(text))
    }
}

impl Deref for SharedText {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_rc()
    }
}

impl AsRef<str> for SharedText {
    fn as_ref(&self) -> &str {
        self
    }
}

impl Borrow<str> for SharedText {
    fn borrow(&self) -> &str {
        self
    }
}

impl fmt::Display for SharedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_ref(), f)
    }
}

impl fmt::Debug for SharedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), f)
    }
}
