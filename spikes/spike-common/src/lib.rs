//! Cripto e log partilhados pelos spikes.
//!
//! Existe para provar uma afirmacao concreta do plano: entre o spike 1 (QUIC pelo iroh) e o
//! spike 3 (onion services pelo arti) muda **so o transporte**. A identidade, as chaves e o
//! log assinado sao literalmente o mesmo codigo, sem uma linha diferente.
//!
//! Se um dia isto precisar de divergir por transporte, e sinal de que a abstracao esta errada.

pub mod crypto;
pub mod log;
