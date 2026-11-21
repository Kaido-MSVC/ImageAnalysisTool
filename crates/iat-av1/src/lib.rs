//! iat-av1: decodes one still AV1 frame with rav1d and hands out its planes as they are.
//!
//! No colour conversion, no upsampling and no range conversion happen here: callers get the
//! Y, U, V planes in the frame's own layout and bit depth. Only [`dav1d`] contains `unsafe`
//! (D-R03, D-R09); this module and the public API are safe.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod dav1d;

use std::fmt;

/// Chroma layout of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    /// Monochrome: only the Y plane.
    I400,
    /// 4:2:0: chroma halved in both directions.
    I420,
    /// 4:2:2: chroma halved horizontally.
    I422,
    /// 4:4:4: full-size chroma.
    I444,
}

/// Decoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Largest frame in pixels rav1d accepts (`frame_size_limit`); 0 means no limit.
    pub frame_size_limit: u32,
}

impl Default for DecodeOptions {
    /// libavif's `AVIF_DEFAULT_IMAGE_SIZE_LIMIT` (16384 × 16384).
    fn default() -> Self {
        Self { frame_size_limit: 16384 * 16384 }
    }
}

/// Why a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// rav1d could not allocate.
    OutOfMemory {
        /// The `dav1d_*` call that failed.
        during: &'static str,
    },
    /// rav1d returned an error code.
    Failed {
        /// The `dav1d_*` call that failed.
        during: &'static str,
        /// The negative `DAV1D_ERR` value (0 when rav1d gave none).
        code: i32,
    },
    /// All data was consumed without a frame coming out.
    NoPicture,
    /// The picture rav1d returned is not usable.
    Malformed(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory { during } => write!(f, "{during}: out of memory"),
            Self::Failed { during, code } => write!(f, "{during} failed ({code})"),
            Self::NoPicture => f.write_str("no frame decoded from the data"),
            Self::Malformed(what) => write!(f, "unusable picture: {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Size of plane `plane` (0 = Y, 1 = U, 2 = V): chroma sizes round up, as in AV1.
fn plane_size(layout: Layout, width: u32, height: u32, plane: usize) -> (u32, u32) {
    if plane == 0 {
        return (width, height);
    }
    match layout {
        Layout::I420 => (width.div_ceil(2), height.div_ceil(2)),
        Layout::I422 => (width.div_ceil(2), height),
        Layout::I444 | Layout::I400 => (width, height),
    }
}

/// One plane of a [`Picture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Plane {
    /// Luma (or gray).
    Y,
    /// First chroma plane (Cb).
    U,
    /// Second chroma plane (Cr).
    V,
}

impl Plane {
    fn index(self) -> usize {
        match self {
            Self::Y => 0,
            Self::U => 1,
            Self::V => 2,
        }
    }
}

/// A decoded frame. Samples stay in rav1d's buffer until the picture is dropped.
pub struct Picture(dav1d::RawPicture);

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("bit_depth", &self.bit_depth())
            .field("layout", &self.layout())
            .field("full_range", &self.full_range())
            .finish()
    }
}

impl Picture {
    /// Frame width in pixels.
    pub fn width(&self) -> u32 {
        self.0.width()
    }
    /// Frame height in pixels.
    pub fn height(&self) -> u32 {
        self.0.height()
    }
    /// 8, 10 or 12.
    pub fn bit_depth(&self) -> u8 {
        self.0.bit_depth()
    }
    /// Chroma layout.
    pub fn layout(&self) -> Layout {
        self.0.layout()
    }
    /// `color_range` of the sequence header (true = full range).
    pub fn full_range(&self) -> bool {
        self.0.full_range()
    }
    /// Whether the frame has `plane` (monochrome frames only have Y).
    pub fn has_plane(&self, plane: Plane) -> bool {
        plane == Plane::Y || self.layout() != Layout::I400
    }
    /// Width and height of `plane`.
    pub fn plane_size(&self, plane: Plane) -> (u32, u32) {
        plane_size(self.layout(), self.width(), self.height(), plane.index())
    }

    /// Copies row `y` of `plane` into `out` as sample values (8-bit samples widen to u16).
    ///
    /// # Panics
    ///
    /// When the frame does not have `plane`, `y` is past the plane's height, or `out` is not
    /// exactly the plane's width.
    pub fn read_row(&self, plane: Plane, y: u32, out: &mut [u16]) {
        assert!(self.has_plane(plane), "{plane:?} plane requested from a {:?} frame", self.layout());
        let (w, _) = self.plane_size(plane);
        assert_eq!(out.len(), w as usize, "row buffer length");
        let bytes = self.0.row_bytes(plane.index(), y);
        if self.bit_depth() > 8 {
            let (pairs, _) = bytes.as_chunks::<2>();
            for (o, &b) in out.iter_mut().zip(pairs) {
                *o = u16::from_ne_bytes(b);
            }
        } else {
            for (o, &b) in out.iter_mut().zip(bytes) {
                *o = u16::from(b);
            }
        }
    }
}

/// Decodes the first frame of an AV1 bitstream (the OBUs of one AVIF item).
///
/// # Errors
///
/// [`DecodeError`] when rav1d rejects the data, runs out of memory, or returns no frame.
pub fn decode_still(obus: &[u8], opts: &DecodeOptions) -> Result<Picture, DecodeError> {
    dav1d::decode(obus, opts).map(Picture)
}

/// rav1d's version string.
pub fn rav1d_version() -> &'static str {
    rav1d::src::lib::rav1d_version()
}
