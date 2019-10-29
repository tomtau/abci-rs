use std::{
    io::{self, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

#[cfg(all(unix, feature = "uds"))]
use std::os::unix::net::UnixListener;

use crate::{
    proto::{abci::*, decode, encode},
    utils::ConsensusState,
    Address, Consensus, Info, Mempool,
};

/// ABCI Server
pub struct SyncServer<C, M, I>
where
    C: Consensus,
    M: Mempool,
    I: Info,
{
    consensus: Arc<C>,
    mempool: Arc<M>,
    info: Arc<I>,
    consensus_state: Arc<Mutex<ConsensusState>>,
}

impl<C, M, I> SyncServer<C, M, I>
where
    C: Consensus + 'static,
    M: Mempool + 'static,
    I: Info + 'static,
{
    /// Creates a new instance of [`Server`](struct.Server.html)
    #[inline]
    pub fn new(consensus: C, mempool: M, info: I) -> Self {
        Self {
            consensus: Arc::new(consensus),
            mempool: Arc::new(mempool),
            info: Arc::new(info),
            consensus_state: Arc::new(Mutex::new(ConsensusState::default())),
        }
    }

    /// Starts ABCI Server
    pub fn start<T: Into<Address>>(&self, addr: T) -> io::Result<()> {
        let addr = addr.into();

        match addr {
            Address::Tcp(addr) => {
                let listener = TcpListener::bind(addr)?;
                log::info!("Started ABCI server at {}", addr);

                for stream in listener.incoming() {
                    self.handle_connection(stream?);
                }
            }
            #[cfg(all(unix, feature = "uds"))]
            Address::Uds(path) => {
                let listener = UnixListener::bind(&path)?;
                log::info!("Started ABCI server at {}", path.display());

                for stream in listener.incoming() {
                    self.handle_connection(stream?);
                }
            }
        }

        Ok(())
    }

    fn handle_connection<S>(&self, mut stream: S)
    where
        S: Read + Write + Send + 'static,
    {
        let consensus = self.consensus.clone();
        let mempool = self.mempool.clone();
        let info = self.info.clone();
        let consensus_state = self.consensus_state.clone();

        thread::spawn(move || loop {
            match decode(&mut stream) {
                Ok(Some(request)) => {
                    log::trace!("Received request: {:?}", request);

                    let value = match request.value.unwrap() {
                        Request_oneof_value::echo(request) => {
                            let mut response = ResponseEcho::new();
                            response.message = info.echo(request.message);
                            Response_oneof_value::echo(response)
                        }
                        Request_oneof_value::flush(_) => {
                            consensus.flush();
                            Response_oneof_value::flush(ResponseFlush::new())
                        }
                        Request_oneof_value::info(request) => {
                            Response_oneof_value::info(info.info(request.into()).into())
                        }
                        Request_oneof_value::set_option(request) => {
                            Response_oneof_value::set_option(info.set_option(request.into()).into())
                        }
                        Request_oneof_value::init_chain(request) => {
                            consensus_state
                                .lock()
                                .unwrap()
                                .validate(ConsensusState::InitChain);
                            Response_oneof_value::init_chain(
                                consensus.init_chain(request.into()).into(),
                            )
                        }
                        Request_oneof_value::query(request) => {
                            Response_oneof_value::query(info.query(request.into()).into())
                        }
                        Request_oneof_value::begin_block(request) => {
                            consensus_state
                                .lock()
                                .unwrap()
                                .validate(ConsensusState::BeginBlock);
                            Response_oneof_value::begin_block(
                                consensus.begin_block(request.into()).into(),
                            )
                        }
                        Request_oneof_value::check_tx(request) => {
                            Response_oneof_value::check_tx(mempool.check_tx(request.into()).into())
                        }
                        Request_oneof_value::deliver_tx(request) => {
                            consensus_state
                                .lock()
                                .unwrap()
                                .validate(ConsensusState::DeliverTx);
                            Response_oneof_value::deliver_tx(
                                consensus.deliver_tx(request.into()).into(),
                            )
                        }
                        Request_oneof_value::end_block(request) => {
                            consensus_state
                                .lock()
                                .unwrap()
                                .validate(ConsensusState::EndBlock);
                            Response_oneof_value::end_block(
                                consensus.end_block(request.into()).into(),
                            )
                        }
                        Request_oneof_value::commit(_) => {
                            consensus_state
                                .lock()
                                .unwrap()
                                .validate(ConsensusState::Commit);
                            Response_oneof_value::commit(consensus.commit().into())
                        }
                    };

                    respond(&mut stream, value);
                }
                Ok(None) => log::trace!("Received empty request"),
                Err(e) => panic!("Error while receiving ABCI request from socket: {}", e),
            }
        });
    }
}

fn respond<W: Write>(writer: W, value: Response_oneof_value) {
    let mut response = Response::new();
    response.value = Some(value);

    log::trace!("Sending response: {:?}", response);

    if let Err(err) = encode(response, writer) {
        log::error!("Error while writing to stream: {}", err);
    }
}