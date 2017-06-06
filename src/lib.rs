#![deny(warnings)]
#![cfg_attr(test, deny(warnings))]

//! A small library for reading and writing BMP images.
//!
//! The library supports uncompressed BMP Version 3 images.
//! The different decoding and encoding schemes is shown in the table below.
//!
//! |Scheme | Decoding | Encoding | Compression |
//! |-------|----------|----------|-------------|
//! | 24 bpp| ✓        | ✓        | No          |
//! | 8 bpp | ✓        | ✗        | No          |
//! | 4 bpp | ✓        | ✗        | No          |
//! | 1 bpp | ✓        | ✗        | No          |
//!
//! # Example
//!
//! ```
//! #[macro_use]
//! extern crate bmp;
//! use bmp::{Image, RGB};
//!
//! fn main() {
//!     let mut img = Image::new(256, 256);
//!
//!     for (x, y) in img.coordinates() {
//!         img.set_pixel(x, y, px!(x, y, 200));
//!     }
//!     let _ = img.save("img.bmp");
//! }
//! ```
//!

extern crate byteorder;

use std::convert::AsRef;
use std::fmt;
use std::fs;
use std::io;
use std::io::{Cursor, Read, Write};
use std::iter::Iterator;

// Expose decoder's public types, structs, and enums
pub use decoder::{BmpError, BmpErrorKind, BmpResult};

/// Macro to generate a `RGB` from `r`, `g` and `b` values.
#[macro_export]
macro_rules! px {
    ($r:expr, $g:expr, $b:expr) => {
        RGB($r as u8, $g as u8, $b as u8)
    }
}

macro_rules! file_size {
    ($bpp:expr, $width:expr, $height:expr) => {{
        let header_size = 2 + 12 + 40;
        // find row size in bytes, round up to 4 bytes (padding)
        let row_size = (($bpp as f32 * $width as f32 + 31.0) / 32.0).floor() as u32 * 4;
        (header_size as u32, $height as u32 * row_size)
    }}
}

/// Common color constants accessible by names.
pub mod consts;

mod decoder;
mod encoder;

/// The color data used for each pixel in the `Image`.
///
/// It has three values for the `red`, `blue` and `green` color channels, respectively.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    /// Creates a new `Color`.
    pub fn new(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b }
    }
}

pub trait Pixel : Clone + Copy + Default + Eq + PartialEq + fmt::Debug {
    fn to_color(&self) -> Color;

    fn from_color(Color) -> Self;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GrayScale(pub u8);

impl Pixel for GrayScale {
    fn to_color(&self) -> Color {
        Color::new(self.0, self.0, self.0)
    }

    fn from_color(color: Color) -> GrayScale {
        let r = 0.299 * color.r as f64;
        let g = 0.587 * color.g as f64;
        let b = 0.114 * color.b as f64;
        GrayScale((r + g + b) as u8)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RGB(pub u8, pub u8, pub u8);

impl Pixel for RGB {
    fn to_color(&self) -> Color {
        Color::new(self.0, self.1, self.2)
    }

    fn from_color(color: Color) -> RGB {
        RGB(color.r, color.g, color.b)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BmpVersion {
    Two,
    Three,
    ThreeNT,
    Four,
    Five,
}

impl BmpVersion {
    fn from_dib_header(dib_header: &BmpDibHeader) -> Option<BmpVersion> {
        match dib_header.header_size {
            12 => Some(BmpVersion::Two),
            40 if dib_header.compress_type == 3 => Some(BmpVersion::ThreeNT),
            40 => Some(BmpVersion::Three),
            108 => Some(BmpVersion::Four),
            124 => Some(BmpVersion::Five),
            _ => None,
        }
    }
}

impl AsRef<str> for BmpVersion {
    fn as_ref(&self) -> &str {
        match *self {
            BmpVersion::Two => "BMP Version 2",
            BmpVersion::Three => "BMP Version 3",
            BmpVersion::ThreeNT => "BMP Version 3 NT",
            BmpVersion::Four => "BMP Version 4",
            BmpVersion::Five => "BMP Version 5",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CompressionType {
    Uncompressed,
    Rle8bit,
    Rle4bit,
    // Only for BMP version 4
    BitfieldsEncoding,
}

impl CompressionType {
    fn from_u32(val: u32) -> CompressionType {
        match val {
            1 => CompressionType::Rle8bit,
            2 => CompressionType::Rle4bit,
            3 => CompressionType::BitfieldsEncoding,
            _ => CompressionType::Uncompressed,
        }
    }
}

impl AsRef<str> for CompressionType {
    fn as_ref(&self) -> &str {
        match *self {
            CompressionType::Rle8bit           => "RLE 8-bit",
            CompressionType::Rle4bit           => "RLE 4-bit",
            CompressionType::BitfieldsEncoding => "Bitfields Encoding",
            CompressionType::Uncompressed      => "Uncompressed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BmpHeader {
    file_size: u32,
    creator1: u16,
    creator2: u16,
    pixel_offset: u32,
}

impl BmpHeader {
    fn new(header_size: u32, data_size: u32) -> BmpHeader {
        BmpHeader {
            file_size: header_size + data_size,
            creator1: 0 /* Unused */,
            creator2: 0 /* Unused */,
            pixel_offset: header_size,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BmpDibHeader {
    header_size: u32,
    width: i32,
    height: i32,
    num_planes: u16,
    bits_per_pixel: u16,
    compress_type: u32,
    data_size: u32,
    hres: i32,
    vres: i32,
    num_colors: u32,
    num_imp_colors: u32,
}

impl BmpDibHeader {
    fn new(width: i32, height: i32) -> BmpDibHeader {
        let (_, pixel_array_size) = file_size!(24.0, width, height);
        BmpDibHeader {
            header_size: 40,
            width: width,
            height: height,
            num_planes: 1,
            bits_per_pixel: 24,
            compress_type: 0,
            data_size: pixel_array_size,
            hres: 1000,
            vres: 1000,
            num_colors: 0,
            num_imp_colors: 0
        }
    }
}

/// The image type provided by the library.
///
/// It exposes functions to initialize or read BMP images from disk, common modification of pixel
/// data, and saving to disk.
///
/// The image is accessed in row-major order from top to bottom,
/// where point (0, 0) is defined to be in the upper left corner of the image.
///
/// Currently, only uncompressed BMP images are supported.
#[derive(Clone, Eq, PartialEq)]
pub struct Image<T: Pixel> {
    header: BmpHeader,
    dib_header: BmpDibHeader,
    color_palette: Option<Vec<Color>>,
    width: u32,
    height: u32,
    padding: u32,
    data: Vec<T>,
}

impl<T: Pixel> Image<T> {
    /// Returns a new BMP Image with the `width` and `height` specified. It is initialized to
    /// a black image by default.
    ///
    /// # Example
    ///
    /// ```
    /// use bmp::{Image, RGB};
    ///
    /// let mut img = Image::<RGB>::new(100, 80);
    /// ```
    pub fn new(width: u32, height: u32) -> Image<T> {
        let (header_size, data_size) = file_size!(24, width, height);
        Image {
            header: BmpHeader::new(header_size, data_size),
            dib_header: BmpDibHeader::new(width as i32, height as i32),
            color_palette: None,
            width,
            height,
            padding: width % 4,
            data: vec![T::default(); (width * height) as usize],
        }
    }

    /// Returns the `width` of the Image.
    #[inline]
    pub fn get_width(&self) -> u32 {
        self.width
    }

    /// Returns the `height` of the Image.
    #[inline]
    pub fn get_height(&self) -> u32 {
        self.height
    }

    /// Set the pixel value at the position of `width` and `height`.
    ///
    /// # Example
    ///
    /// ```
    /// use bmp::{Image, consts};
    ///
    /// let mut img = Image::new(100, 80);
    /// img.set_pixel(10, 10, consts::RED);
    /// ```
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, val: T) {
        self.data[((self.height - y - 1) * self.width + x) as usize] = val;
    }

    /// Returns the pixel value at the position of `width` and `height`.
    ///
    /// # Example
    ///
    /// ```
    /// use bmp::{Image, consts};
    ///
    /// let img = Image::new(100, 80);
    /// assert_eq!(consts::BLACK, img.get_pixel(10, 10));
    /// ```
    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> T {
        self.data[((self.height - y - 1) * self.width + x) as usize]
    }

    /// Returns a new `ImageIndex` that iterates over the image dimensions in top-bottom order.
    ///
    /// # Example
    ///
    /// ```
    /// use bmp::{Image, consts};
    ///
    /// let mut img = Image::new(100, 100);
    /// for (x, y) in img.coordinates() {
    ///     img.set_pixel(x, y, consts::BLUE);
    /// }
    /// ```
    #[inline]
    pub fn coordinates(&self) -> ImageIndex {
        ImageIndex::new(self.width as u32, self.height as u32)
    }

    /// Saves the image to the path specified by `name`. The function will overwrite the contents
    /// if a file already exists with the same name.
    ///
    /// The function returns the `io::Result` from the underlying `Reader`.
    ///
    /// # Example
    ///
    /// ```
    /// use bmp::{Image, RGB};
    ///
    /// let mut img = Image::<RGB>::new(100, 100);
    /// let _ = img.save("black.bmp").unwrap_or_else(|e| {
    ///     panic!("Failed to save: {}", e)
    /// });
    /// ```
    pub fn save(&self, name: &str) -> io::Result<()> {
        let bmp_data = encoder::encode_image(self)?;
        let mut bmp_file = fs::File::create(name)?;
        bmp_file.write(&bmp_data)?;
        Ok(())
    }
}

impl<T: Pixel> fmt::Debug for Image<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "Image {}\n", '{')?;
        write!(f, "\theader: {:?},\n", self.header)?;
        write!(f, "\tdib_header: {:?},\n", self.dib_header)?;
        write!(f, "\tcolor_palette: {:?},\n", self.color_palette)?;
        write!(f, "\twidth: {:?},\n", self.width)?;
        write!(f, "\theight: {:?},\n", self.height)?;
        write!(f, "\tpadding: {:?},\n", self.padding)?;
        write!(f, "{}", '}')
    }
}

/// An `Iterator` returning the `x` and `y` coordinates of an image.
///
/// It supports iteration over an image in row-major order,
/// starting from in the upper left corner of the image.
#[derive(Clone, Copy)]
pub struct ImageIndex {
    width: u32,
    height: u32,
    x: u32,
    y: u32
}

impl ImageIndex {
    fn new(width: u32, height: u32) -> ImageIndex {
        ImageIndex {
            width,
            height,
            x: 0,
            y: 0
        }
    }
}

impl Iterator for ImageIndex {
    type Item = (u32, u32);

    fn next(&mut self) -> Option<(u32, u32)> {
        if self.x < self.width && self.y < self.height {
            let this = Some((self.x, self.y));
            self.x += 1;
            if self.x == self.width {
                self.x = 0;
                self.y += 1;
            }
            this
        } else {
            None
        }
    }
}

/// Returns a `BmpResult`, either containing an `Image` or a `BmpError`.
///
/// # Example
///
/// ```
/// use bmp::RGB;
///
/// let img = bmp::open::<RGB>("test/rgbw.bmp").unwrap_or_else(|e| {
///    panic!("Failed to open: {}", e);
/// });
/// ```
pub fn open<T: Pixel>(name: &str) -> BmpResult<Image<T>> {
    let mut bytes = Vec::new();
    let mut f = fs::File::open(name)?;
    f.read_to_end(&mut bytes)?;
    let mut bmp_data = Cursor::new(bytes);

    decoder::decode_image(&mut bmp_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    use std::mem::size_of;

    #[test]
    fn size_of_bmp_header_is_54_bytes() {
        let bmp_header_size = size_of::<BmpHeader>();
        let bmp_bip_header_size = size_of::<BmpDibHeader>();

        assert_eq!(12, bmp_header_size);
        assert_eq!(40, bmp_bip_header_size);
    }

    // #[test]
    // fn size_of_4pixel_bmp_image_is_70_bytes() {
    //     let path_wd = path::Path::new("test/rgbw.bmp");
    //     match path_wd.metadata() {
    //         Ok(stat) => assert_eq!(70, stat.len() as i32),
    //         Err(_) => (/* Ignore IoError for now */)
    //     }
    // }

    fn verify_test_bmp_image(img: Image<RGB>) {
        let header = img.header;
        assert_eq!(70, header.file_size);
        assert_eq!(0,  header.creator1);
        assert_eq!(0,  header.creator2);

        let dib_header = img.dib_header;
        assert_eq!(54,   header.pixel_offset);
        assert_eq!(40,   dib_header.header_size);
        assert_eq!(2,    dib_header.width);
        assert_eq!(2,    dib_header.height);
        assert_eq!(1,    dib_header.num_planes);
        assert_eq!(24,   dib_header.bits_per_pixel);
        assert_eq!(0,    dib_header.compress_type);
        assert_eq!(16,   dib_header.data_size);
        assert_eq!(1000, dib_header.hres);
        assert_eq!(1000, dib_header.vres);
        assert_eq!(0,    dib_header.num_colors);
        assert_eq!(0,    dib_header.num_imp_colors);

        assert_eq!(2, img.padding);
    }

    #[test]
    fn can_read_bmp_image() {
        let bmp_img = open("test/rgbw.bmp").unwrap();
        verify_test_bmp_image(bmp_img);
    }

    #[test]
    fn can_read_image_data() {
        let mut f = fs::File::open("test/rgbw.bmp").unwrap();
        f.seek(SeekFrom::Start(54)).unwrap();

        let mut px = [0; 3];
        f.read(&mut px).unwrap();

        assert_eq!(px!(px[2], px[1], px[0]), consts::BLUE);
    }

    #[test]
    fn can_read_entire_bmp_image() {
        let bmp_img = open::<RGB>("test/rgbw.bmp").unwrap();
        assert_eq!(bmp_img.data.len(), 4);

        assert_eq!(bmp_img.get_pixel(0, 0), consts::RED);
        assert_eq!(bmp_img.get_pixel(1, 0), consts::LIME);
        assert_eq!(bmp_img.get_pixel(0, 1), consts::BLUE);
        assert_eq!(bmp_img.get_pixel(1, 1), consts::WHITE);
    }

    #[test]
    fn read_write_1pbb_bmp_image() {
        let img = open::<RGB>("test/bmptestsuite-0.9/valid/1bpp-1x1.bmp").unwrap();
        assert_eq!(img.data.len(), 1);
        assert_eq!(img.get_pixel(0, 0), consts::BLACK);

        let _ = img.save("test/1bb-1x1.bmp");
        let img = open::<RGB>("test/1bb-1x1.bmp").unwrap();
        assert_eq!(img.data.len(), 1);
        assert_eq!(img.get_pixel(0, 0), consts::BLACK);
    }

    #[test]
    fn read_write_4pbb_bmp_image() {
        let img = open::<RGB>("test/bmptestsuite-0.9/valid/4bpp-1x1.bmp").unwrap();
        assert_eq!(img.data.len(), 1);
        assert_eq!(img.get_pixel(0, 0), consts::BLUE);

        let _ = img.save("test/4bb-1x1.bmp");
        let img = open::<RGB>("test/4bb-1x1.bmp").unwrap();
        assert_eq!(img.data.len(), 1);
        assert_eq!(img.get_pixel(0, 0), consts::BLUE);
    }

    #[test]
    fn read_write_8pbb_bmp_image() {
        let img = open::<RGB>("test/bmptestsuite-0.9/valid/8bpp-1x1.bmp").unwrap_or_else(|e| {
            panic!("{}", e);
        });
        assert_eq!(img.data.len(), 1);
        assert_eq!(img.get_pixel(0, 0), consts::BLUE);

        let _ = img.save("test/8bb-1x1.bmp");
        let img = open::<RGB>("test/8bb-1x1.bmp").unwrap_or_else(|e| {
            panic!("{}", e);
        });
        assert_eq!(img.data.len(), 1);
        assert_eq!(img.get_pixel(0, 0), consts::BLUE);
    }

    #[test]
    fn error_when_opening_unexisting_image() {
        let result = open::<RGB>("test/no_img.bmp");
        match result {
            Err(BmpError{ kind: BmpErrorKind::BmpIoError(_), .. }) => (/* Expected */),
            _ => panic!("Ghost image!?")
        }
    }

    #[test]
    fn error_when_opening_image_with_wrong_bits_per_pixel() {
        let result = open::<RGB>("test/bmptestsuite-0.9/valid/32bpp-1x1.bmp");
        match result {
            Err(BmpError { kind: BmpErrorKind::UnsupportedBitsPerPixel, .. }) => (/* Expected */),
            _ => panic!("32bpp should not be supported")
        }
    }

    #[test]
    fn error_when_opening_image_with_wrong_magic_numbers() {
        let result = open::<RGB>("test/bmptestsuite-0.9/corrupt/magicnumber-bad.bmp");
        match result {
            Err(BmpError { kind: BmpErrorKind::WrongMagicNumbers, .. }) => (/* Expected */),
            _ => panic!("Wrong magic numbers should not be supported")
        }
    }

    #[test]
    fn can_create_bmp_file() {
        let mut bmp = Image::new(2, 2);
        bmp.set_pixel(0, 0, consts::RED);
        bmp.set_pixel(1, 0, consts::LIME);
        bmp.set_pixel(0, 1, consts::BLUE);
        bmp.set_pixel(1, 1, consts::WHITE);
        let _ = bmp.save("test/rgbw_test.bmp");

        let bmp_img = open::<RGB>("test/rgbw_test.bmp").unwrap();
        assert_eq!(bmp_img.get_pixel(0, 0), consts::RED);
        assert_eq!(bmp_img.get_pixel(1, 0), consts::LIME);
        assert_eq!(bmp_img.get_pixel(0, 1), consts::BLUE);
        assert_eq!(bmp_img.get_pixel(1, 1), consts::WHITE);

        verify_test_bmp_image(bmp_img);
    }

    #[test]
    fn changing_pixels_does_not_push_image_data() {
        let mut img = Image::new(2, 1);
        img.set_pixel(1, 0, consts::WHITE);
        img.set_pixel(0, 0, consts::WHITE);

        assert_eq!(img.get_pixel(0, 0), consts::WHITE);
        assert_eq!(img.get_pixel(1, 0), consts::WHITE);
    }

    #[test]
    fn coordinates_iterator_gives_x_and_y_in_row_major_order() {
        let img = Image::<RGB>::new(2, 3);
        let mut coords = img.coordinates();
        assert_eq!(coords.next(), Some((0, 0)));
        assert_eq!(coords.next(), Some((1, 0)));
        assert_eq!(coords.next(), Some((0, 1)));
        assert_eq!(coords.next(), Some((1, 1)));
        assert_eq!(coords.next(), Some((0, 2)));
        assert_eq!(coords.next(), Some((1, 2)));
    }

    // TODO: Add benches when they are considered stable
    // #[bench]
    // fn write_bmp(b: &mut test::Bencher) {
    //     let img = Image::new(320, 240);
    //     b.iter(|| img.save("test/bench_test.bmp"));
    // }

    // #[bench]
    // fn open_bmp(b: &mut test::Bencher) {
    //     b.iter(|| open("test/bmptestsuite-0.9/valid/24bpp-320x240.bmp"));
    // }
}
