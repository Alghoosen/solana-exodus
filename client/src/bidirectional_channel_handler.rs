use {
    crossbeam_channel::{Receiver, Sender},
    log::debug,
    quinn::RecvStream,
    solana_sdk::signature::Signature,
    solana_streamer::bidirectional_channel::{
        QuicReplyMessage, QUIC_REPLY_MESSAGE_OFFSET, QUIC_REPLY_MESSAGE_SIGNATURE_OFFSET,
        QUIC_REPLY_MESSAGE_SIZE,
    },
    std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    },
};

pub const PACKET_DATA_SIZE: usize = 1280 - 40 - 8;

// This structure will handle the bidirectional messages that we get from the quic server
// It will save 1024 QuicReplyMessages sent by the server in the crossbeam receiver
// This class will also handle recv channel created by the QuicClient when connecting to the server in bidirectional mode
#[derive(Clone)]
pub struct BidirectionalChannelHandler {
    sender: Arc<Sender<QuicReplyMessage>>,
    pub reciever: Receiver<QuicReplyMessage>,
    recv_channel_is_set: Arc<AtomicBool>,
}

impl BidirectionalChannelHandler {
    pub fn new() -> Self {
        let (sender, reciever) = crossbeam_channel::unbounded();
        Self {
            sender: Arc::new(sender),
            reciever,
            recv_channel_is_set: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_serving(&self) -> bool {
        self.recv_channel_is_set.load(Ordering::Relaxed)
    }

    pub fn start_serving(&self, recv_stream: RecvStream) {
        if self.is_serving() {
            return;
        }

        let recv_channel_is_set = self.recv_channel_is_set.clone();
        let sender = self.sender.clone();

        recv_channel_is_set.store(true, Ordering::Relaxed);
        // create task to fetch errors from the leader
        tokio::spawn(async move {
            // wait for 10 s max
            let mut timeout: u64 = 10_000;
            let mut start = Instant::now();

            const LAST_BUFFER_SIZE: usize = QUIC_REPLY_MESSAGE_SIZE + 1;
            let mut last_buffer: [u8; LAST_BUFFER_SIZE] = [0; LAST_BUFFER_SIZE];
            let mut buffer_written = 0;
            let mut recv_stream = recv_stream;
            loop {
                if let Ok(chunk) = tokio::time::timeout(
                    Duration::from_millis(timeout),
                    recv_stream.read_chunk(PACKET_DATA_SIZE, false),
                )
                .await
                {
                    match chunk {
                        Ok(maybe_chunk) => {
                            match maybe_chunk {
                                Some(chunk) => {
                                    // move data into current buffer
                                    let mut buffer = vec![0; buffer_written + chunk.bytes.len()];
                                    if buffer_written > 0 {
                                        // copy remaining data from previous buffer
                                        buffer[0..buffer_written]
                                            .copy_from_slice(&last_buffer[0..buffer_written]);
                                    }
                                    buffer[buffer_written..buffer_written + chunk.bytes.len()]
                                        .copy_from_slice(&chunk.bytes);
                                    buffer_written = buffer_written + chunk.bytes.len();

                                    while buffer_written >= QUIC_REPLY_MESSAGE_SIZE {
                                        let signature = bincode::deserialize::<Signature>(
                                            &buffer[QUIC_REPLY_MESSAGE_SIGNATURE_OFFSET
                                                ..QUIC_REPLY_MESSAGE_OFFSET],
                                        );
                                        let message: [u8; 128] = buffer
                                            [QUIC_REPLY_MESSAGE_OFFSET..QUIC_REPLY_MESSAGE_SIZE]
                                            .try_into()
                                            .unwrap();
                                        if let Ok(signature) = signature {
                                            if let Err(_) =
                                                sender.send(QuicReplyMessage::new_with_bytes(
                                                    signature, message,
                                                ))
                                            {
                                                // crossbeam channel closed
                                                break;
                                            }
                                        } else {
                                            // deserializing error
                                            debug!("deserializing error on BidirectionalChannelHandler");
                                        }
                                        buffer.copy_within(QUIC_REPLY_MESSAGE_SIZE.., 0);
                                        buffer_written -= QUIC_REPLY_MESSAGE_SIZE;
                                    }
                                    if buffer_written > 0 {
                                        // move remianing data into last buffer
                                        last_buffer[0..buffer_written]
                                            .copy_from_slice(&buffer[0..buffer_written]);
                                    }
                                }
                                None => {
                                    // done receiving chunks
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            debug!("BidirectionalChannelHandler recieved error {}", e);
                            break;
                        }
                    }
                } else {
                    break;
                }

                timeout = timeout.saturating_sub((Instant::now() - start).as_millis() as u64);
                start = Instant::now();
            }
            recv_channel_is_set.store(false, Ordering::Relaxed);
            println!("stopping recv stream");
        });
    }
}
