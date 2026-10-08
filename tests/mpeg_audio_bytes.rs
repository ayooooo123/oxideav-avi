//! MPEG audio stored by the byte (`strh.dwSampleSize` 1, `nBlockAlign` 1,
//! `dwRate` the byte rate, as old encoders such as Intel's H.263 capture
//! wrote MP3): the registry's demuxer opens such a file as FFmpeg's avidec
//! does, splits its chunks, which cut through frames, into whole MPEG audio
//! frames as FFmpeg's parser does, and times them by the byte (the first
//! frame after open or a seek carries its time; the decoder's output
//! continues it). The validating `open_avi` still names the mismatch.

use std::io::Cursor;

use oxideav_core::{Error, NullCodecResolver, ReadSeek};

/// An MPEG-2 layer III frame header: 22050 Hz, 32 kbit/s, mono, with or
/// without the padding byte; 104 or 105 bytes long.
fn frame(pad: bool, fill: u8) -> Vec<u8> {
    let mut f = vec![0xFF, 0xF3, if pad { 0x42 } else { 0x40 }, 0xC0];
    f.resize(if pad { 105 } else { 104 }, fill);
    f
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

/// One MP3 stream (`dwScale` 1, `dwRate` 3981, `dwSampleSize`
/// `sample_size`) whose movi holds `stream` cut into chunks of `sizes`
/// (the last takes the rest), indexed by idx1.
fn avi(stream: &[u8], sizes: &[usize], sample_size: u32) -> Vec<u8> {
    let mut avih = Vec::new();
    for v in [40_000u32, 0, 0, 0x10, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0] {
        avih.extend_from_slice(&v.to_le_bytes());
    }
    let mut strh = b"auds\0\0\0\0".to_vec();
    // dwFlags, wPriority + wLanguage, dwInitialFrames, dwScale, dwRate,
    // dwStart, dwLength, dwSuggestedBufferSize, dwQuality, dwSampleSize,
    // rcFrame
    for v in [0u32, 0, 0, 1, 3981, 0, stream.len() as u32, 1845, u32::MAX, sample_size, 0, 0] {
        strh.extend_from_slice(&v.to_le_bytes());
    }
    let mut strf = Vec::new();
    strf.extend_from_slice(&0x0055u16.to_le_bytes());
    strf.extend_from_slice(&1u16.to_le_bytes());
    strf.extend_from_slice(&22_050u32.to_le_bytes());
    strf.extend_from_slice(&3981u32.to_le_bytes());
    strf.extend_from_slice(&1u16.to_le_bytes()); // nBlockAlign
    strf.extend_from_slice(&0u16.to_le_bytes());
    strf.extend_from_slice(&12u16.to_le_bytes());
    strf.extend_from_slice(&[1, 0, 2, 0, 0, 0, 0x68, 0, 1, 0, 0x71, 5]); // MPEGLAYER3WAVEFORMAT
    let mut strl = b"strl".to_vec();
    chunk(&mut strl, b"strh", &strh);
    chunk(&mut strl, b"strf", &strf);
    let mut hdrl = b"hdrl".to_vec();
    chunk(&mut hdrl, b"avih", &avih);
    chunk(&mut hdrl, b"LIST", &strl);
    let mut movi = b"movi".to_vec();
    let mut idx1 = Vec::new();
    let mut at = 0;
    for (i, &n) in sizes.iter().enumerate() {
        let n = if i + 1 == sizes.len() { stream.len() - at } else { n };
        idx1.extend_from_slice(b"00wb");
        for v in [0x10u32, movi.len() as u32, n as u32] {
            idx1.extend_from_slice(&v.to_le_bytes());
        }
        chunk(&mut movi, b"00wb", &stream[at..at + n]);
        at += n;
    }
    let mut body = b"AVI ".to_vec();
    chunk(&mut body, b"LIST", &hdrl);
    chunk(&mut body, b"LIST", &movi);
    chunk(&mut body, b"idx1", &idx1);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// 20 frames, alternately unpadded and padded.
fn frames() -> Vec<Vec<u8>> {
    (0..20u8).map(|i| frame(i % 2 == 1, i)).collect()
}

fn open(file: Vec<u8>) -> Box<dyn oxideav_core::Demuxer> {
    let rs: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    oxideav_avi::demuxer::open(rs, &NullCodecResolver).expect("FFmpeg plays it")
}

fn drain(d: &mut dyn oxideav_core::Demuxer) -> Vec<(Vec<u8>, Option<i64>)> {
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => out.push((p.data, p.pts)),
            Err(Error::Eof) => return out,
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn the_registry_demuxer_opens_a_byte_counted_mp3_stream() {
    let stream: Vec<u8> = frames().concat();
    let d = open(avi(&stream, &[1845], 1));
    assert_eq!(d.streams()[0].params.sample_rate, Some(22_050));
}

#[test]
fn chunks_cut_through_frames_come_out_as_whole_frames() {
    let frames = frames();
    let stream: Vec<u8> = frames.concat();
    let mut d = open(avi(&stream, &[250, 333, 41, 600, 0], 1));
    let got = drain(&mut *d);
    assert_eq!(got.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>(), frames, "every frame whole, in order");
    assert_eq!(got[0].1, Some(0));
    assert!(got[1..].iter().all(|(_, pts)| pts.is_none()), "later frames are the decoder's to time");
}

/// A seek lands on the chunk holding the target byte time and times its
/// first whole frame by the byte it starts at.
#[test]
fn a_seek_times_the_next_frame_by_its_byte() {
    let frames = frames();
    let stream: Vec<u8> = frames.concat();
    let mut d = open(avi(&stream, &[250, 333, 41, 600, 0], 1));
    // byte 583 starts the third chunk (250 + 333); its first whole frame
    // is the sixth (5 * 104.5 rounded: bytes 104+105+104+105+104 = 522,
    // then 627), at byte 627.
    let landed = d.seek_to(0, 600).unwrap();
    assert_eq!(landed, 583, "the chunk's byte time");
    let (data, pts) = d.next_packet().map(|p| (p.data, p.pts)).unwrap();
    assert_eq!((data, pts), (frames[6].clone(), Some(627)));
}

/// `open_avi` validates: it names the dwSampleSize the codec's carriage
/// does not allow, where the registry's demuxer plays the file.
#[test]
fn the_validating_open_still_names_the_mismatch() {
    let stream: Vec<u8> = frames().concat();
    let rs: Box<dyn ReadSeek> = Box::new(Cursor::new(avi(&stream, &[1845], 1)));
    let err = oxideav_avi::demuxer::open_avi(rs, &NullCodecResolver).err().expect("open_avi validates");
    assert!(err.to_string().contains("dwSampleSize"), "{err}");
}
