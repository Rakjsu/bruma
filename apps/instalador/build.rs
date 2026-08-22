//! O build do instalador embute o Bruma dentro dele.
//!
//! O instalador transporta a app comprimida no próprio executável — é por isso que ele
//! não descarrega nada e funciona offline. Consequência de que é preciso gostar: o
//! `bruma.exe` de release tem de existir ANTES de se compilar isto. No CI a ordem já é
//! essa (o tauri-action compila a app primeiro); localmente, se faltar, o erro abaixo
//! diz exatamente o que correr em vez de deixar o cargo queixar-se de um ficheiro que
//! não existe.

use std::path::PathBuf;

fn main() {
    // O alvo do workspace, respeitando CARGO_TARGET_DIR se alguém o tiver mudado.
    let raiz = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let alvo = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| raiz.join("../../target"));
    let bruma = alvo.join("release/bruma.exe");

    println!("cargo:rerun-if-changed={}", bruma.display());
    println!("cargo:rerun-if-changed=../desktop/tauri.conf.json");

    // Sem o bruma.exe compilado, embute-se um payload VAZIO em vez de rebentar: o
    // clippy e os testes do CI verificam o código sem terem a app de release à mão.
    // O runtime recusa-se a instalar um payload vazio — nunca sai um instalador oco
    // por engano, porque o passo de release compila a app primeiro.
    let bytes = std::fs::read(&bruma).unwrap_or_else(|_| {
        println!(
            "cargo:warning=sem {} — instalador de VERIFICAÇÃO, não instala nada",
            bruma.display()
        );
        Vec::new()
    });

    // Nível 19: uns segundos a comprimir uma vez, contra megabytes a menos em cada
    // download de cada pessoa. A troca certa tem lado.
    let comprimido = zstd::encode_all(&bytes[..], 19).expect("compressão falhou");
    let destino = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("bruma.exe.zst");
    std::fs::write(&destino, &comprimido).expect("não consegui escrever o payload");
    println!(
        "cargo:warning=payload: bruma.exe {:.1} MB -> {:.1} MB",
        bytes.len() as f64 / 1e6,
        comprimido.len() as f64 / 1e6
    );

    // A versão vem de um sítio só: o tauri.conf.json da app. O instalador não tem uma
    // versão própria para nunca poder discordar dela.
    let conf: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(raiz.join("../desktop/tauri.conf.json")).unwrap(),
    )
    .unwrap();
    println!(
        "cargo:rustc-env=VERSAO_DA_APP={}",
        conf["version"].as_str().expect("versão em falta")
    );

    tauri_build::build();
}
