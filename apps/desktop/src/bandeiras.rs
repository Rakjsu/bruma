//! As bandeiras de ambiente que existem **só para medir**, num sítio só.
//!
//! # Porque é que isto é um módulo e não uma chamada a `env::var` onde é precisa
//!
//! Este projecto força caminhos que não acontecem sozinhos: um codificador que morre a meio,
//! uma captura que falha, uma sessão que cai, um sync lento de propósito. Sem isso, o código
//! que trata dessas falhas nunca corre num teste — e código que nunca corre não está testado,
//! está escrito.
//!
//! Nada disso tem que ver com **usar** a app. Uma pessoa com o `BRUMA_SEM_TRAVAO` posto por
//! engano fica com o travão da partilha de ecrã desligado e nunca saberá porquê: a app não
//! diz nada, só se comporta mal. Um andaime numa instalação a sério é uma armadilha, não uma
//! ferramenta.
//!
//! Estando aqui, cada nome aparece **uma vez**, dentro de um bloco `#[cfg(debug_assertions)]`.
//! Na release a função devolve o valor neutro e o nome nem sequer existe no binário — o que é
//! verificável com uma busca de bytes, e é o que a ferramenta `so-o-que-vai-na-release` faz.
//!
//! `#[cfg]` e não `cfg!()`: com `cfg!()` o comportamento fica certo mas o texto continua no
//! exe e a chamada ao ambiente é feita na mesma. Já medi as duas metades do mesmo padrão a
//! darem resultados diferentes no mesmo compilador.
//!
//! # O que NÃO está aqui
//!
//! `BRUMA_DADOS` (escolher a pasta de dados) e `BRUMA_REGISTO` (nível de registo) são
//! diagnóstico legítimo, estão documentados no README, e servem a quem instalou a app quando
//! alguma coisa corre mal. Ficam onde estão.

/// A sessão cai a meio, para se poder medir a religação.
#[cfg(debug_assertions)]
pub fn sessao_morre() -> bool {
    std::env::var("BRUMA_SESSAO_MORRE").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn sessao_morre() -> bool {
    false
}

/// Atrasa o sync de propósito, para a janela em que uma mensagem se pode perder ser
/// observável em vez de instantânea.
#[cfg(debug_assertions)]
pub fn sync_lento_ms() -> Option<u64> {
    std::env::var("BRUMA_SYNC_LENTO").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn sync_lento_ms() -> Option<u64> {
    None
}

/// O codificador de ecrã morre ao fim de N pedaços.
#[cfg(debug_assertions)]
pub fn codificador_morre_ao() -> Option<u64> {
    std::env::var("BRUMA_CODIFICADOR_MORRE").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn codificador_morre_ao() -> Option<u64> {
    None
}

/// A captura de ecrã falha à saída da porta.
#[cfg(debug_assertions)]
pub fn falha_captura() -> bool {
    std::env::var("BRUMA_FALHA_CAPTURA").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn falha_captura() -> bool {
    false
}

/// Desliga o travão da partilha de ecrã.
#[cfg(debug_assertions)]
pub fn sem_travao() -> bool {
    std::env::var("BRUMA_SEM_TRAVAO").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn sem_travao() -> bool {
    false
}

/// A captura continua a correr depois de mandada parar, para se medir a fuga.
#[cfg(debug_assertions)]
pub fn so_vigia() -> bool {
    std::env::var("BRUMA_SO_VIGIA").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn so_vigia() -> bool {
    false
}

/// O som morre ao fim de N segundos.
#[cfg(debug_assertions)]
pub fn som_morre_aos() -> Option<u64> {
    std::env::var("BRUMA_SOM_MORRE").ok()?.parse().ok()
}
#[cfg(not(debug_assertions))]
pub fn som_morre_aos() -> Option<u64> {
    None
}

/// Volta ao caminho de eco antigo, para se provar que o novo o corrigiu.
#[cfg(debug_assertions)]
pub fn eco_antigo() -> bool {
    std::env::var("BRUMA_ECO_ANTIGO").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn eco_antigo() -> bool {
    false
}

/// Captura só o som da própria app em vez do sistema todo.
#[cfg(debug_assertions)]
pub fn so_nos() -> bool {
    std::env::var("BRUMA_SO_NOS").is_ok()
}
#[cfg(not(debug_assertions))]
pub fn so_nos() -> bool {
    false
}
