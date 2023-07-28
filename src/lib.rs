use std::io::{ErrorKind, Read, Seek, Write};
use std::mem::MaybeUninit;
use std::{io, ptr};
use std::ffi::c_void;
use std::io::SeekFrom::{Current, End, Start};
use libc::memmove;
use lzma_sys::{lzma_code, lzma_end, lzma_filter, LZMA_FILTER_LZMA1, lzma_options_lzma, LZMA_PRESET_DEFAULT, lzma_raw_encoder, LZMA_RUN, LZMA_OK, lzma_stream, LZMA_VLI_UNKNOWN, LZMA_MEM_ERROR, LZMA_FINISH, LZMA_STREAM_END, lzma_raw_decoder};

fn write_props<W: Write + ?Sized>(writer: &mut W, options: &lzma_options_lzma) -> io::Result<()> {
    let props = [
        (options.pb as u8 * 5 + options.lp as u8) * 9 + options.lc as u8,
        options.dict_size as u8,
        (options.dict_size >> 8) as u8,
        (options.dict_size >> 16) as u8,
        (options.dict_size >> 24) as u8
    ];
    writer.write_all(&props)
}

fn read_props<R: Read + ?Sized>(reader: &mut R, options: &mut lzma_options_lzma) -> io::Result<()> {
    let mut props = [0;5];
    reader.read_exact(&mut props)?;
    options.dict_size = ((props[4] as u32) << 24) | ((props[3] as u32) << 16) | ((props[2] as u32) << 8) | (props[1] as u32);
    let mut x = props[0];
    options.lc = (x % 9) as u32;
    x /= 9;
    options.lp = (x % 5) as u32;
    x /= 5;
    options.pb = x as u32;
    Ok(())
}

unsafe fn lzma_stream_raw<R: Read + ?Sized, W: Write + ?Sized>(reader: &mut R, writer: &mut W, stream: *mut lzma_stream) -> io::Result<()> {
    let mut input = [0u8; 65536];
    let mut output = [0u8; 65536];
    loop {
        (*stream).next_in = input.as_ptr();
        let count = reader.read(&mut input[(*stream).avail_in as usize..])?;
        if count == 0 { break; }
        (*stream).avail_in += count;
        (*stream).next_out = output.as_mut_ptr();
        (*stream).avail_out = output.len();
        let result = unsafe { lzma_code(stream, LZMA_RUN) };
        match result {
            LZMA_OK => {},
            LZMA_MEM_ERROR => panic!("Out of memory"),
            x => return Err(io::Error::new(ErrorKind::Other, format!("LZMA Error {x}")))
        }
        let output_count = unsafe { (*stream).next_out.offset_from(output.as_ptr()) } as usize;
        writer.write_all(&output[0..output_count])?;
        unsafe { memmove(input.as_mut_ptr() as *mut c_void, (*stream).next_in as *const c_void, (*stream).avail_in as usize); }
    }
    loop {
        let result = unsafe { lzma_code(stream, LZMA_FINISH) };
        match result {
            LZMA_OK => {},
            LZMA_STREAM_END => {},
            LZMA_MEM_ERROR => panic!("Out of memory"),
            x => return Err(io::Error::new(ErrorKind::Other, format!("LZMA Error {x}")))
        }
        let output_count = unsafe { (*stream).next_out.offset_from(output.as_ptr()) } as usize;
        writer.write_all(&output[0..output_count])?;
        unsafe { memmove(input.as_mut_ptr() as *mut c_void, (*stream).next_in as *const c_void, (*stream).avail_in as usize); }
        if result == LZMA_STREAM_END { break; }
    }
    Ok(())
}

fn lzma_compress_raw<R: Read + ?Sized, W: Write + ?Sized>(reader: &mut R, writer: &mut W, filter: *const lzma_filter) -> io::Result<()> {
    let mut lzma_stream = MaybeUninit::<lzma_stream>::zeroed();
    unsafe {
        let ptr = lzma_stream.as_mut_ptr();
        lzma_raw_encoder(ptr, filter);
        let result = lzma_stream_raw(reader, writer, ptr);
        lzma_end(ptr);
        result
    }
}

fn lzma_decompress_raw<R: Read + ?Sized, W: Write + ?Sized>(reader: &mut R, writer: &mut W, filter: *const lzma_filter) -> io::Result<()> {
    let mut lzma_stream = MaybeUninit::<lzma_stream>::zeroed();
    unsafe {
        let ptr = lzma_stream.as_mut_ptr();
        lzma_raw_decoder(ptr, filter);
        let result = lzma_stream_raw(reader, writer, ptr);
        lzma_end(ptr);
        result
    }
}

pub fn lzma_compress<R: Read + Seek + ?Sized, W: Write + Seek + ?Sized>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    let mut filter = Vec::<lzma_sys::lzma_filter>::new();
    let mut lzma1_options = MaybeUninit::<lzma_options_lzma>::zeroed();
    let mut lzma1_options = unsafe {
        let ptr = lzma1_options.as_mut_ptr();
        lzma_sys::lzma_lzma_preset(ptr, LZMA_PRESET_DEFAULT);
        lzma1_options.assume_init()
    };
    let ptr_lzma1_options: *mut _ = &mut lzma1_options;
    filter.push(lzma_sys::lzma_filter { id: LZMA_FILTER_LZMA1, options: ptr_lzma1_options as *mut c_void });
    filter.push(lzma_sys::lzma_filter { id: LZMA_VLI_UNKNOWN, options: ptr::null_mut() });
    write_props(writer, &lzma1_options)?;
    let input_position = reader.stream_position()?;
    let input_length = reader.seek(End(0))? - input_position;
    reader.seek(Start(input_position))?;
    writer.write_all(&input_length.to_le_bytes())?;
    let compressed_length_position = writer.seek(Current(0))?;
    writer.write_all(&[0;8])?;
    let writer_start = writer.seek(Current(0))?;
    lzma_compress_raw(reader, writer, filter.as_ptr())?;
    let writer_end = writer.seek(Current(0))?;
    writer.seek(Start(compressed_length_position))?;
    writer.write_all(&(writer_end - writer_start).to_le_bytes())?;
    writer.seek(Start(writer_end))?;
    Ok(())
}

struct LimitReader<'a> {
    reader: Box<dyn Read + 'a>,
    left: usize
}

impl Read for LimitReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.len() < self.left {
            let count = self.reader.read(buf)?;
            self.left -= count;
            Ok(count)
        } else {
            let count = self.reader.read(&mut buf[0..self.left])?;
            self.left -= count;
            Ok(count)
        }
    }
}

impl<'a> LimitReader<'a> {
    fn new<R: Read + 'a>(reader: R, length: usize) -> Self {
        Self {
            reader: Box::new(reader) as Box<dyn Read>,
            left: length
        }
    }
}

pub fn lzma_decompress<R: Read + ?Sized, W: Write + ?Sized>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    let mut filter = Vec::<lzma_sys::lzma_filter>::new();
    let mut lzma1_options = MaybeUninit::<lzma_options_lzma>::zeroed();
    let mut lzma1_options = unsafe {
        let ptr = lzma1_options.as_mut_ptr();
        lzma_sys::lzma_lzma_preset(ptr, LZMA_PRESET_DEFAULT);
        lzma1_options.assume_init()
    };
    let ptr_lzma1_options: *mut _ = &mut lzma1_options;
    filter.push(lzma_sys::lzma_filter { id: LZMA_FILTER_LZMA1, options: ptr_lzma1_options as *mut c_void });
    filter.push(lzma_sys::lzma_filter { id: LZMA_VLI_UNKNOWN, options: ptr::null_mut() });
    read_props(reader, &mut lzma1_options)?;
    let mut uncompressed_length = [0;8];
    reader.read_exact(&mut uncompressed_length)?;
    let uncompressed_length = u64::from_le_bytes(uncompressed_length);
    let mut compressed_length = [0;8];
    reader.read_exact(&mut compressed_length)?;
    let compressed_length = u64::from_le_bytes(compressed_length);
    let mut reader = LimitReader::new(reader, compressed_length as usize);
    lzma_decompress_raw(&mut reader, writer, filter.as_ptr())?;
    Ok(())
}
