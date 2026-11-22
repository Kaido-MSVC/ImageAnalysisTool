//! The only module with `unsafe`: calls into rav1d's `dav1d_*` API and owns its handles.
//!
//! Every handle is wrapped in a type whose `Drop` releases it, and every pointer handed out by
//! rav1d is turned into a slice only after checking it against the picture's own geometry.
#![allow(unsafe_code)]

use std::mem::MaybeUninit;
use std::ptr::{self, NonNull};
use std::slice;

use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dLogger, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422, DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture, dav1d_open,
    dav1d_picture_unref, dav1d_send_data,
};

use crate::{DecodeError, DecodeOptions, Layout};

/// `DAV1D_ERR(EAGAIN)`: rav1d negates the errno value; `libc::EAGAIN` is 11 on Windows.
const ERR_EAGAIN: i32 = -11;
/// `DAV1D_ERR(ENOMEM)`: `libc::ENOMEM` is 12 on Windows.
const ERR_ENOMEM: i32 = -12;

fn error_of(code: i32, during: &'static str) -> DecodeError {
    if code == ERR_ENOMEM { DecodeError::OutOfMemory { during } } else { DecodeError::Failed { during, code } }
}

/// An open decoder context; closed on drop.
struct Context(Option<Dav1dContext>);

impl Context {
    fn open(opts: &DecodeOptions) -> Result<Self, DecodeError> {
        let mut s = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: `dav1d_default_settings` only writes a complete `Dav1dSettings` to the pointer,
        // which points to writable memory of the right size and alignment.
        unsafe { dav1d_default_settings(NonNull::from(&mut s).cast()) };
        // SAFETY: the call above initialized every field.
        let mut s = unsafe { s.assume_init() };
        // Same as libavif's dav1d glue with maxThreads = 1: one thread, one frame in flight.
        s.n_threads = 1;
        s.max_frame_delay = 1;
        s.frame_size_limit = opts.frame_size_limit;
        s.operating_point = 0;
        s.all_layers = 0;
        // No logger: rav1d's default writes to stderr, which the worker forwards as `worker_stderr`.
        // SAFETY: `Dav1dLogger::new` only requires a callback that is safe to call; with no callback
        // (and no cookie) rav1d treats logging as disabled and never calls anything.
        s.logger = unsafe { Dav1dLogger::new(None, None) };
        let mut ctx: Option<Dav1dContext> = None;
        // SAFETY: both pointers come from live, exclusive references; `dav1d_open` reads the
        // settings and writes the context (or `None` on failure).
        let r = unsafe { dav1d_open(Some(NonNull::from(&mut ctx)), Some(NonNull::from(&mut s))) };
        if r.0 != 0 {
            return Err(error_of(r.0, "dav1d_open"));
        }
        match ctx {
            Some(c) => Ok(Self(Some(c))),
            None => Err(DecodeError::Failed { during: "dav1d_open", code: 0 }),
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if self.0.is_some() {
            // SAFETY: the context came from `dav1d_open` and is closed only here; `dav1d_close`
            // sets the option to `None`.
            unsafe { dav1d_close(Some(NonNull::from(&mut self.0))) };
        }
    }
}

/// Input data owned by rav1d (a copy of the caller's bytes); released on drop.
struct Data(Dav1dData);

impl Data {
    fn copy_of(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError::Failed { during: "dav1d_data_create", code: 0 });
        }
        let mut d = MaybeUninit::<Dav1dData>::uninit();
        // SAFETY: `d` is writable memory for one `Dav1dData`; on success rav1d writes a complete
        // value and returns a pointer to a fresh buffer of `bytes.len()` bytes.
        let dst = unsafe { dav1d_data_create(Some(NonNull::from(&mut d).cast()), bytes.len()) };
        if dst.is_null() {
            return Err(DecodeError::OutOfMemory { during: "dav1d_data_create" });
        }
        // SAFETY: `dav1d_data_create` returned non-null, so it initialized `d`.
        let d = unsafe { d.assume_init() };
        // SAFETY: `dst` is a fresh allocation of `bytes.len()` bytes owned by `d`, which cannot
        // overlap the caller's slice.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len()) };
        Ok(Self(d))
    }

    fn remaining(&self) -> usize {
        if self.0.data.is_some() { self.0.sz } else { 0 }
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid `Dav1dData` (fully consumed data has a null pointer and
        // unref is then a no-op).
        unsafe { dav1d_data_unref(Some(NonNull::from(&mut self.0))) };
    }
}

/// A decoded picture; its reference is released on drop.
pub(crate) struct RawPicture {
    p: Dav1dPicture,
    layout: Layout,
    bit_depth: u8,
    full_range: bool,
    width: u32,
    height: u32,
}

impl Drop for RawPicture {
    fn drop(&mut self) {
        // SAFETY: `self.p` was written by a successful `dav1d_get_picture` and is released only here.
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut self.p))) };
    }
}

/// Fetches one picture; `Ok(None)` is EAGAIN.
fn get_picture(ctx: &Context) -> Result<Option<Dav1dPicture>, DecodeError> {
    let mut p = MaybeUninit::<Dav1dPicture>::uninit();
    // SAFETY: the context is open; `p` is writable memory for one `Dav1dPicture`, which rav1d
    // always writes (an empty picture on failure).
    let r = unsafe { dav1d_get_picture(ctx.0, Some(NonNull::from(&mut p).cast())) };
    // SAFETY: written by the call above in every case.
    let mut p = unsafe { p.assume_init() };
    match r.0 {
        0 => Ok(Some(p)),
        code => {
            // SAFETY: an empty picture: unref is a no-op, but releases anything rav1d might have set.
            unsafe { dav1d_picture_unref(Some(NonNull::from(&mut p))) };
            if code == ERR_EAGAIN { Ok(None) } else { Err(error_of(code, "dav1d_get_picture")) }
        }
    }
}

/// Decodes the first frame of `obus` the way libavif's dav1d glue does (send / get loop, then
/// drain and drop any further frames).
pub(crate) fn decode(obus: &[u8], opts: &DecodeOptions) -> Result<RawPicture, DecodeError> {
    let ctx = Context::open(opts)?;
    let mut data = Data::copy_of(obus)?;
    let picture = loop {
        if data.remaining() > 0 {
            // SAFETY: the context is open and `data.0` is a valid `Dav1dData` that rav1d may
            // update in place (it advances past consumed bytes).
            let r = unsafe { dav1d_send_data(ctx.0, Some(NonNull::from(&mut data.0))) };
            if r.0 < 0 && r.0 != ERR_EAGAIN {
                return Err(error_of(r.0, "dav1d_send_data"));
            }
        }
        match get_picture(&ctx)? {
            Some(p) => break p,
            None if data.remaining() > 0 => continue,
            None => return Err(DecodeError::NoPicture),
        }
    };
    let picture = wrap(picture)?;
    // Drain: later frames are dropped, as libavif does; a hard error here still fails.
    while let Some(mut extra) = get_picture(&ctx)? {
        // SAFETY: a picture from a successful `dav1d_get_picture`, released once.
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut extra))) };
    }
    picture.check_planes()?;
    Ok(picture)
}

fn wrap(p: Dav1dPicture) -> Result<RawPicture, DecodeError> {
    let mut raw = RawPicture { p, layout: Layout::I400, bit_depth: 0, full_range: false, width: 0, height: 0 };
    let Some(seq) = raw.p.seq_hdr else {
        return Err(DecodeError::Malformed("picture without a sequence header"));
    };
    // SAFETY: `seq_hdr` points to the sequence header the picture holds a reference to; it lives
    // as long as the picture.
    let color_range = unsafe { seq.as_ref() }.color_range;
    raw.full_range = color_range != 0;
    raw.layout = match raw.p.p.layout {
        DAV1D_PIXEL_LAYOUT_I400 => Layout::I400,
        DAV1D_PIXEL_LAYOUT_I420 => Layout::I420,
        DAV1D_PIXEL_LAYOUT_I422 => Layout::I422,
        DAV1D_PIXEL_LAYOUT_I444 => Layout::I444,
        _ => return Err(DecodeError::Malformed("unknown pixel layout")),
    };
    raw.bit_depth = match raw.p.p.bpc {
        8 => 8,
        10 => 10,
        12 => 12,
        _ => return Err(DecodeError::Malformed("unsupported bit depth")),
    };
    raw.width = u32::try_from(raw.p.p.w).ok().filter(|&w| w > 0).ok_or(DecodeError::Malformed("bad width"))?;
    raw.height = u32::try_from(raw.p.p.h).ok().filter(|&h| h > 0).ok_or(DecodeError::Malformed("bad height"))?;
    Ok(raw)
}

impl RawPicture {
    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }
    pub(crate) fn bit_depth(&self) -> u8 {
        self.bit_depth
    }
    pub(crate) fn full_range(&self) -> bool {
        self.full_range
    }
    pub(crate) fn width(&self) -> u32 {
        self.width
    }
    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    fn bytes_per_sample(&self) -> usize {
        if self.bit_depth > 8 { 2 } else { 1 }
    }

    /// Stride in bytes and size of plane `plane` (0 = Y, 1 = U, 2 = V).
    fn plane_geometry(&self, plane: usize) -> (isize, u32, u32) {
        let (w, h) = crate::plane_size(self.layout, self.width, self.height, plane);
        (self.p.stride[usize::from(plane > 0)], w, h)
    }

    /// Checks once that every plane pointer, stride and alignment covers the plane, so that
    /// `row_bytes` can slice without further checks.
    fn check_planes(&self) -> Result<(), DecodeError> {
        let planes = if self.layout == Layout::I400 { 1 } else { 3 };
        for plane in 0..planes {
            let (stride, w, h) = self.plane_geometry(plane);
            let row = usize::try_from(w).ok().and_then(|w| w.checked_mul(self.bytes_per_sample()));
            let Some(row) = row else { return Err(DecodeError::Malformed("plane row too large")) };
            let stride_ok = usize::try_from(stride).is_ok_and(|s| s >= row);
            let total = usize::try_from(stride).ok().and_then(|s| s.checked_mul(usize::try_from(h).ok()?));
            let Some(base) = self.p.data[plane] else { return Err(DecodeError::Malformed("missing plane")) };
            if !stride_ok || total.is_none_or(|t| t > isize::MAX as usize) {
                return Err(DecodeError::Malformed("plane stride does not cover a row"));
            }
            if self.bytes_per_sample() == 2 && (!(base.as_ptr() as usize).is_multiple_of(2) || stride % 2 != 0) {
                return Err(DecodeError::Malformed("16-bit plane is not aligned"));
            }
        }
        Ok(())
    }

    /// The bytes of row `y` of `plane` (`width × bytes per sample` long).
    ///
    /// # Panics
    ///
    /// When `plane` or `y` is out of range (a bug in the caller; `Picture` checks first).
    pub(crate) fn row_bytes(&self, plane: usize, y: u32) -> &[u8] {
        let (stride, w, h) = self.plane_geometry(plane);
        assert!(y < h, "row {y} out of range (plane {plane} has {h} rows)");
        let base = self.p.data[plane].expect("plane pointer checked in check_planes");
        let len = w as usize * self.bytes_per_sample();
        // `check_planes` showed stride >= 0 and stride × h fits in isize.
        let offset = stride.unsigned_abs() * y as usize;
        // SAFETY: rav1d allocated each plane with `stride` bytes per row for at least `h` rows
        // (checked in `check_planes`), so `base + offset .. + len` lies inside the plane; the
        // picture holds a reference to the buffer until `self` is dropped, and nothing writes to
        // it meanwhile (the decoder context is already closed).
        unsafe { slice::from_raw_parts(base.as_ptr().cast::<u8>().add(offset), len) }
    }
}
