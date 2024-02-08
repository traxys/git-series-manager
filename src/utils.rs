use std::mem::{self, ManuallyDrop};

/// Allows transmuting between types of different sizes.
///
/// Necessary for transmuting in generic functions, since (as of Rust 1.51.0)
/// transmute doesn't work well with generic types.
///
/// # Safety
///
/// This function has the same safety requirements as [`std::mem::transmute_copy`].
///
/// # Example
///
/// ```rust
/// use core_extensions::utils::transmute_ignore_size;
///
/// use std::mem::MaybeUninit;
///
/// unsafe fn transmute_into_init<T>(array: [MaybeUninit<T>; 3]) -> [T; 3] {
///     transmute_ignore_size(array)
/// }
///
/// let array = [MaybeUninit::new(3), MaybeUninit::new(5), MaybeUninit::new(8)];
///
/// unsafe{ assert_eq!(transmute_into_init(array), [3, 5, 8]); }
///
/// ```
///
/// This is the error you get if you tried to use `std::mem::transmute`.
///
/// ```text
/// error[E0512]: cannot transmute between types of different sizes, or dependently-sized types
///  --> src/lib.rs:4:5
///   |
/// 4 |     std::mem::transmute(array)
///   |     ^^^^^^^^^^^^^^^^^^^
///   |
///   = note: source type: `[MaybeUninit<T>; 3]` (size can vary because of T)
///   = note: target type: `[T; 3]` (size can vary because of T)
/// ```
///
///
/// [`std::mem::transmute_copy`]: https://doc.rust-lang.org/std/mem/fn.transmute_copy.html
#[inline(always)]
pub unsafe fn transmute_ignore_size<T, U>(v: T) -> U {
    let v = ManuallyDrop::new(v);
    mem::transmute_copy::<T, U>(&v)
}

pub trait TypeIdent {
    type Type: ?Sized;

    #[inline(always)]
    fn into_type(self) -> Self::Type
    where
        Self: Sized,
        Self::Type: Sized,
    {
        unsafe { transmute_ignore_size(self) }
    }
}

impl<T: ?Sized> TypeIdent for T {
    type Type = T;
}

pub trait OptExt<T>: TypeIdent<Type = Option<T>> + Sized {
    fn try_unwrap_or_else<F, E>(self, f: F) -> Result<T, E>
    where
        F: Fn() -> Result<T, E>,
    {
        self.into_type().map(Ok::<T, E>).unwrap_or_else(f)
    }

    fn try_m_unwrap_or_else<F>(self, f: F) -> Result<T, miette::Report>
    where
        F: Fn() -> Result<T, miette::Report>,
    {
        self.try_unwrap_or_else(f)
    }
}

impl<T> OptExt<T> for Option<T> {}
