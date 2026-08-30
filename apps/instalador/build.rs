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

    // O tamanho DESCOMPRIMIDO, para o EstimatedSize do registo dizer a verdade (#181).
    //
    // O registo anunciava o tamanho do payload comprimido — metade do que a instalação
    // ocupa de facto. É no «Adicionar/Remover Programas» que as pessoas vão ver quanto
    // espaço uma app come antes de decidirem o que desinstalar; anunciar 7 MB e ocupar 40
    // é mentir no sítio onde se tomam decisões.
    println!("cargo:rustc-env=TAMANHO_DA_APP={}", bytes.len());

    // A versão vem de um sítio só: o tauri.conf.json da app. O instalador não tem uma
    // versão própria para nunca poder discordar dela.
    let conf: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(raiz.join("../desktop/tauri.conf.json")).unwrap(),
    )
    .unwrap();
    let versao = conf["version"].as_str().expect("versão em falta");
    println!("cargo:rustc-env=VERSAO_DA_APP={versao}");

    // E O RECURSO DE VERSÃO DO EXE DIZ A MESMA (#181).
    //
    // O tauri-build carimba no exe a versão do tauri.conf.json DESTE crate — que dizia
    // 0.1.0 desde sempre. As Propriedades → Detalhes de qualquer Instalar-Bruma.exe, de
    // qualquer release, diziam 0.1.0 para sempre: precisamente onde uma pessoa vai
    // confirmar «que versão é este ficheiro que eu descarreguei?».
    //
    // Sincroniza-se AQUI, antes do tauri_build ler o ficheiro, e escreve-se só quando
    // difere — a árvore só fica suja no dia do bump, que é quando há mesmo um commit a
    // fazer. E o portão da release confere as quatro cópias, portanto uma dessincronização
    // nunca chega a publicar-se.
    let conf_meu = raiz.join("tauri.conf.json");
    let texto = std::fs::read_to_string(&conf_meu).unwrap();
    let mut j: serde_json::Value = serde_json::from_str(&texto).unwrap();
    if j["version"].as_str() != Some(versao) {
        j["version"] = serde_json::Value::String(versao.to_string());
        std::fs::write(&conf_meu, serde_json::to_string_pretty(&j).unwrap() + "\n").unwrap();
        println!("cargo:warning=tauri.conf.json do instalador sincronizado para {versao}");
    }

    tauri_build::build();
}
