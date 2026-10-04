use ech_db_protocol::errors::{DbError, DbErrorCode, RetryClass};
use ech_db_protocol::hash::Blake2b256;
use ech_db_protocol::ids::{Stream, StreamId};
use ech_db_protocol::keys::{LogKey, Prefix, Range, ReadKey, StateKey, SysKey, WindowKey};
use ech_db_protocol::values::{Head, ProgramBundle, ProgramChunks};
use ech_db_protocol::wire::{
    Chunk, ChunkAssembler, ChunkSplitter, ClientStreamItem, Open, OpenResponse, Request, Response,
    ServerStreamItem,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect()
}

fn root() -> [u8; 32] {
    [0x11; 32]
}

fn stream() -> StreamId {
    Stream::intent([0x22; 32]).id()
}

#[test]
fn stream_id_vectors() {
    assert_eq!(
        hex(Stream::intent([0u8; 32]).id().as_array()),
        "7063dfebe3c8561c3c7d9311df3a1dee422ae47b9b82ef5b228744aafd33138f"
    );
    assert_eq!(
        hex(Stream::program([0u8; 32]).id().as_array()),
        "732b70e124bba3f742776a5d7962791014e2c92e1d7e44e11f5ce6889d638fbd"
    );
}

#[test]
fn program_hash_vector() {
    let bundle = ProgramBundle {
        format_version: 1,
        execution_profile: 1,
        wasm: vec![0xaa, 0xbb],
    };
    let encoded = bcs::to_bytes(&bundle).unwrap();
    assert_eq!(hex(&encoded), "010000000100000002aabb");
    let hash = bundle.hash().unwrap();
    assert_eq!(
        hex(&hash),
        "c603f8616615fdd27da1a7574bfd75017f2bbbcdc4c5d31b163044cb4a9ed509"
    );
    assert_eq!(hash, Blake2b256::hash(&[encoded.as_slice()]));
}

#[test]
fn log_and_state_key_vectors() {
    assert_eq!(
        hex(&LogKey::seq(&root(), &stream())),
        "01111111111111111111111111111111111111111111111111111111111111111100026c6f6700018b1b1b43894424a2c4cdb9f51db26f970453beef618614697fc92ff89bdbdf40000273657100"
    );
    assert_eq!(
        hex(&LogKey::data(&root(), &stream(), 5)),
        "01111111111111111111111111111111111111111111111111111111111111111100026c6f6700018b1b1b43894424a2c4cdb9f51db26f970453beef618614697fc92ff89bdbdf40000264617461001505"
    );
    assert_eq!(
        hex(&StateKey::prefix(&root())),
        "0111111111111111111111111111111111111111111111111111111111111111110002737461746500"
    );
    assert_eq!(
        hex(&SysKey::head(&root())),
        "011111111111111111111111111111111111111111111111111111111111111111000273797300026865616400"
    );
    assert_eq!(
        hex(&SysKey::window_number(&root(), 3)),
        "0111111111111111111111111111111111111111111111111111111111111111110002737973000277696e646f77001503"
    );
}

#[test]
fn window_record_versionstamp_vector() {
    let key = WindowKey::record(&root(), 0, 2);
    assert_eq!(
        hex(&key),
        "011111111111111111111111111111111111111111111111111111111111111111000277696e646f77001433ffffffffffffffffffff00022c000000"
    );
}

#[test]
fn read_key_suffix_vectors() {
    let key = ReadKey::log_data(&stream(), 5);
    assert_eq!(
        hex(&key.suffix),
        "018b1b1b43894424a2c4cdb9f51db26f970453beef618614697fc92ff89bdbdf40000264617461001505"
    );
    let physical = key.physical(&root());
    assert_eq!(hex(&physical), hex(&LogKey::data(&root(), &stream(), 5)));
}

#[test]
fn prefix_successor_vectors() {
    assert_eq!(Prefix::successor(&[1, 2, 0xff, 0xff]).unwrap(), vec![1, 3]);
    assert_eq!(Prefix::successor(&[0xff, 0xff]), None);
    assert_eq!(Prefix::successor(&[]), None);
    assert_eq!(Prefix::successor(&[0x00]), Some(vec![0x01]));
}

#[test]
fn range_is_contained_in_namespace() {
    let range = Range::log_data(&stream());
    let (begin, end) = range.physical(&root());
    assert!(begin < end);
    let data_key = LogKey::data(&root(), &stream(), 10);
    let seq_key = LogKey::seq(&root(), &stream());
    assert!(data_key >= begin && data_key < end);
    assert!(seq_key < begin || seq_key >= end);
}

#[test]
fn bcs_message_vectors() {
    let begin = bcs::to_bytes(&Request::BeginRead).unwrap();
    assert_eq!(hex(&begin), "03");
    let close = bcs::to_bytes(&Request::CloseRead).unwrap();
    assert_eq!(hex(&close), "06");
    let opened = bcs::to_bytes(&Response::ReadOpened).unwrap();
    assert_eq!(hex(&opened), "03");
    let closed = bcs::to_bytes(&Response::ReadClosed).unwrap();
    assert_eq!(hex(&closed), "06");
    let error = Response::Error(DbError::never(DbErrorCode::Integrity));
    assert_eq!(hex(&bcs::to_bytes(&error).unwrap()), "07000e");
    let chunk = Chunk {
        message_id: 1,
        bytes: vec![0x0a],
        last: true,
    };
    assert_eq!(hex(&bcs::to_bytes(&chunk).unwrap()), "0100000000000000010a01");
    let retry = Response::Error(DbError::fdb(1020, RetryClass::Retry));
    assert_eq!(hex(&bcs::to_bytes(&retry).unwrap()), "07010ffc030000");
}

#[test]
fn open_and_auth_vectors() {
    let open = Open {
        protocol_version: 1,
        auth: ech_db_protocol::wire::Auth {
            pubkey: [0u8; 32],
            signature: [0u8; 64],
        },
    };
    let encoded = bcs::to_bytes(&open).unwrap();
    assert_eq!(encoded.len(), 4 + 32 + 64);
    assert_eq!(&encoded[..4], &[1, 0, 0, 0]);
    let ack = OpenResponse::Opened;
    assert_eq!(hex(&bcs::to_bytes(&ack).unwrap()), "00");
}

#[test]
fn chunk_assembler_round_trip() {
    let message = vec![0x5a; Chunk::MAX_BYTES * 2 + 17];
    let mut splitter = ChunkSplitter::new();
    let chunks = splitter.split(&message);
    assert_eq!(chunks.len(), 3);
    assert!(chunks.iter().all(|chunk| chunk.bytes.len() <= Chunk::MAX_BYTES));
    assert_eq!(chunks[0].message_id, 0);
    assert_eq!(chunks[1].message_id, 0);
    assert_eq!(chunks[2].message_id, 0);
    assert!(!chunks[0].last && !chunks[1].last && chunks[2].last);
    let mut assembler = ChunkAssembler::new();
    let mut assembled = None;
    for chunk in &chunks {
        if let Some(message) = assembler.feed(chunk).unwrap() {
            assembled = Some(message);
        }
    }
    assert_eq!(assembled.unwrap(), message);
    let bad = Chunk {
        message_id: 7,
        bytes: vec![1],
        last: true,
    };
    assert!(assembler.feed(&bad).is_err());
    let mut second = ChunkSplitter::new();
    let chunks = second.split(&[]);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].last);
}

#[test]
fn stream_items_are_marked() {
    let open = ClientStreamItem::Open(Open {
        protocol_version: 1,
        auth: ech_db_protocol::wire::Auth {
            pubkey: [0u8; 32],
            signature: [0u8; 64],
        },
    });
    let response = ServerStreamItem::OpenResponse(OpenResponse::Opened);
    match (open, response) {
        (ClientStreamItem::Open(_), ServerStreamItem::OpenResponse(_)) => {}
        _ => panic!("unreachable"),
    }
}

#[test]
fn program_chunk_split_vectors() {
    let payload = vec![7u8; 115_000];
    let chunks = ProgramChunks::split(&payload);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].len(), 50 * 1024);
    assert_eq!(chunks[1].len(), 50 * 1024);
    assert_eq!(chunks[2].len(), 115_000 - 100 * 1024);
}

#[test]
fn head_window_vectors() {
    let head = Head::initial(1_000_000, 600_000).unwrap();
    assert_eq!(head.number, 0);
    assert_eq!(head.closes_at_unix_ms, 1_200_000);
    assert!(!head.is_due(1_199_999));
    assert!(head.is_due(1_200_000));
    let next = head.next(600_000).unwrap();
    assert_eq!(next.number, 1);
    assert_eq!(next.closes_at_unix_ms, 1_800_000);
}

#[test]
fn stream_id_depends_on_kind_source() {
    let id_one = Stream::intent([1u8; 32]).id();
    let id_two = Stream::intent([2u8; 32]).id();
    assert_ne!(id_one, id_two);
    let id_again = Stream::intent([1u8; 32]).id();
    assert_eq!(id_one, id_again);
    assert_ne!(id_one, Stream::program([1u8; 32]).id());
    let _ = from_hex("00");
}
