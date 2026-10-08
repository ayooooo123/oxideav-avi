//! Type-1 DV AVI (one `iavs`/`ivas` stream of whole DIF frames, as
//! FFmpeg's avidec reads it): the stream is the DV video, and its audio
//! comes out too, as a `dvaudio` stream of the same frames, timed from the
//! first frame after open or a seek on (the decoder knows how many samples
//! each frame holds).

use std::io::Cursor;

use oxideav_core::{CodecId, CodecInfo, CodecRegistry, CodecTag, Demuxer, Error, MediaType, ReadSeek};

fn chunk(fcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = fcc.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = kind.to_vec();
    b.extend_from_slice(body);
    chunk(b"LIST", &b)
}

/// A type-1 DV AVI of `frames` (one `00__` chunk each, idx1 indexed),
/// stream type `fcc_type`, handler `handler`, 25 frames/s; `extra` chunks
/// go after the frames inside movi.
fn type1(fcc_type: &[u8; 4], handler: &[u8; 4], frames: &[Vec<u8>], extra: &[u8]) -> Vec<u8> {
    let n = frames.len() as u32;
    let mut avih = Vec::new();
    for v in [40_000u32, 0, 0, 0x110, n, 0, 1, 0, 720, 576, 0, 0, 0, 0] {
        avih.extend_from_slice(&v.to_le_bytes());
    }
    let mut strh = fcc_type.to_vec();
    strh.extend_from_slice(handler);
    for v in [0u32, 0, 0, 1, 25, 0, n, 0, u32::MAX, 0] {
        strh.extend_from_slice(&v.to_le_bytes());
    }
    for v in [0i16, 0, 720, 576] {
        strh.extend_from_slice(&v.to_le_bytes());
    }
    let strl = [chunk(b"strh", &strh), chunk(b"strf", &[0; 32])].concat();
    let hdrl = list(b"hdrl", &[chunk(b"avih", &avih), list(b"strl", &strl)].concat());
    let mut movi = Vec::new();
    let mut idx1 = Vec::new();
    for f in frames {
        idx1.extend_from_slice(b"00__");
        for v in [0x10u32, 4 + movi.len() as u32, f.len() as u32] {
            idx1.extend_from_slice(&v.to_le_bytes());
        }
        movi.extend_from_slice(&chunk(b"00__", f));
    }
    movi.extend_from_slice(extra);
    let body = [b"AVI ".to_vec(), hdrl, list(b"movi", &movi), chunk(b"idx1", &idx1)].concat();
    [b"RIFF".to_vec(), (body.len() as u32).to_le_bytes().to_vec(), body].concat()
}

fn frames() -> Vec<Vec<u8>> {
    (0..3u8).map(|i| vec![i + 1; 1000 + i as usize]).collect()
}

fn open(file: Vec<u8>) -> Box<dyn Demuxer> {
    let mut reg = CodecRegistry::new();
    reg.register(CodecInfo::new(CodecId::new("dvvideo")).tags([CodecTag::fourcc(b"dvsd"), CodecTag::fourcc(b"dvhd")]));
    let rs: Box<dyn ReadSeek> = Box::new(Cursor::new(file));
    oxideav_avi::demuxer::open(rs, &reg).unwrap()
}

/// (stream, first byte, pts) of every packet.
fn drain(d: &mut dyn Demuxer) -> Vec<(u32, u8, Option<i64>)> {
    let mut out = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => out.push((p.stream_index, p.data[0], p.pts)),
            Err(Error::Eof) => return out,
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn the_dv_stream_is_its_video_and_its_audio() {
    let d = open(type1(b"iavs", b"dvsd", &frames(), &[]));
    let s = d.streams();
    assert_eq!(s.len(), 2);
    assert_eq!((s[0].params.media_type, s[0].params.codec_id.as_str()), (MediaType::Video, "dvvideo"));
    assert_eq!((s[1].params.media_type, s[1].params.codec_id.as_str()), (MediaType::Audio, "dvaudio"));
    assert_eq!(s[1].params.tag, Some(CodecTag::fourcc(b"dvsd")), "audio carried in dvsd frames");
    assert_eq!(s[1].index, 1);
    assert_eq!((s[1].time_base, s[1].duration), (s[0].time_base, s[0].duration));
    assert_eq!(s[0].duration, Some(3));
}

#[test]
fn each_frame_comes_out_on_both_streams() {
    let f = frames();
    let mut d = open(type1(b"iavs", b"dvsd", &f, &[]));
    assert_eq!(
        drain(&mut *d),
        vec![(0, 1, Some(0)), (1, 1, Some(0)), (0, 2, Some(1)), (1, 2, None), (0, 3, Some(2)), (1, 3, None)]
    );
}

#[test]
fn a_seek_times_the_next_audio_packet() {
    let mut d = open(type1(b"iavs", b"dvsd", &frames(), &[]));
    drain(&mut *d);
    assert_eq!(d.seek_to(0, 2).unwrap(), 2);
    assert_eq!(drain(&mut *d), vec![(0, 3, Some(2)), (1, 3, Some(2))]);
    // A seek on the audio stream is a seek on its frames.
    assert_eq!(d.seek_to(1, 1).unwrap(), 1);
    assert_eq!(drain(&mut *d)[..2], [(0, 2, Some(1)), (1, 2, Some(1))]);
}

#[test]
fn ivas_with_a_dvhd_handler_too() {
    let d = open(type1(b"ivas", b"dvhd", &frames(), &[]));
    assert_eq!(d.streams().len(), 2);
    assert_eq!(d.streams()[1].params.tag, Some(CodecTag::fourcc(b"dvhd")));
}

/// No chunk addresses the audio stream: a stray `01wb` is not one of its
/// packets.
#[test]
fn a_chunk_numbered_for_the_audio_stream_is_skipped() {
    let mut d = open(type1(b"iavs", b"dvsd", &frames(), &chunk(b"01wb", &[9; 10])));
    let got = drain(&mut *d);
    assert_eq!(got.len(), 6);
    assert!(got.iter().all(|&(_, byte, _)| byte != 9));
}

#[test]
fn another_handler_keeps_the_stream_as_data() {
    let d = open(type1(b"iavs", b"xvid", &frames(), &[]));
    assert_eq!(d.streams().len(), 1);
    assert_eq!(d.streams()[0].params.media_type, MediaType::Data);
}
