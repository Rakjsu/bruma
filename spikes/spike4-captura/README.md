# Spike 4 — captura nativa, sem passar pelo `getDisplayMedia`

**Estado: PASSA, e já saiu daqui.** Os gates passaram todos, e o caminho está em produção
desde a v0.5.0 — o `getDisplayMedia` deixou de ser usado e a barra do WebView2 desapareceu
com ele. O que se aprendeu a traduzir entre o Media Foundation e o MSE está em
`apps/desktop/src/mse.rs`, com testes.

```bash
cargo run -p spike4-captura --release -- 10
```

## Porque é que este spike existe

A partilha de ecrã vivia dentro da WebView2, e isso trazia duas coisas que não se
resolvem por configuração:

1. **A barra "está a partilhar uma janela"**, desenhada por cima da app. Fui à
   documentação da Microsoft antes de assumir que era um detalhe: o único gancho é o
   evento `ScreenCaptureStarting`, e ele tem apenas `Cancel` (bloqueia a partilha toda) e
   `Handled` (decide qual handler corre primeiro). Nenhum esconde a barra, e não há flag
   de linha de comando. E faz sentido que não haja — a barra existe precisamente para
   nenhuma aplicação capturar o teu ecrã sem tu veres. Não é um enfeite do WebView2, é o
   indicador de segurança dele.
2. **O Spike 2 mediu o AV1 a correr por software** (libaom) dentro do Chromium. A placa
   desta máquina tem codificador em hardware e ele não estava a ser usado.

Capturar e codificar em Rust resolve as duas de uma vez: o WebView2 deixa de ter o que
anunciar, e passamos a escolher o codificador.

## A decisão de desenho

**Não se monta uma segunda pilha de WebRTC.** Já existe entre pares um transporte
autenticado e cifrado — o iroh. O caminho curto é capturar em Rust, codificar em Rust,
mandar os NALs pelo iroh, e descodificar do outro lado com o `VideoDecoder` do WebCodecs,
que a WebView2 tem (medido abaixo — que o use por hardware é outra pergunta, ainda aberta).

O que isso ganha:

- dispensa ICE e DTLS só para o vídeo;
- dispensa o TURN nesta parte — o iroh tem relay próprio;
- **tira a partilha de ecrã da lista de coisas que revelam o IP.** O WebRTC fazia o seu
  próprio furo no NAT, por fora do iroh, e era essa a limitação nº 1 do plano. Deixa de
  se aplicar ao vídeo;
- encaixa na árvore de distribuição do orçamento de upload: quem reencaminha move bytes
  cifrados que não consegue ler.

O preço é assumido e não se disfarça: **perdemos o controlo de congestão do WebRTC** e
passamos a ter de fazer o nosso. O plano já queria esse controlo — é o capítulo do
orçamento de upload — mas é trabalho que passa a ser obrigatório, não opcional.

## O que foi medido

Máquina: Windows 11, ecrã 3440×1440 a 175 Hz, GPU NVIDIA. Três corridas, cena real
(navegador com vídeo e um jogo abertos), ~67% do ecrã a mudar a cada frame.

| | frames | ritmo | intervalo p50/p95 | débito |
|---|---|---|---|---|
| só a captar | 578 em 10,2 s | **56,9 fps** | 17,2 / 18,4 ms | — |
| a codificar | 577 em 10,6 s | **54,5 fps** | 17,2 / 18,7 ms | 7,1 Mbps |

Codificadores de H.264 que o Media Foundation oferece nesta máquina:

```
[hardware] NVIDIA H.264 Encoder MFT
[software] Microsoft AVC DX12 Encoder
[software] H264 Encoder MFT
```

### Gates

| | resultado |
|---|---|
| G4 · sai em pedaços, não só no fim | **PASSA** — primeiro aos 319 ms, 50 pedaços em 5 s |
| G4 · cadência serve para ver ao vivo | **PASSA** — pior intervalo 329 ms |
| G4 · é mesmo MP4 fragmentado | **PASSA** — `ftyp uuid pdin moov moof mdat moof mdat …` |
| G1 · existe codificador por hardware | **PASSA** — `NVIDIA H.264 Encoder MFT` |
| G1 · e o nosso caminho usa-o mesmo | **PASSA** — 6–7% de uso do NVENC a codificar, 0% a não codificar |
| G2 · captura chega aos 50 fps | **PASSA** — 56,9 fps |
| G2 · regiões sujas disponíveis | **PASSA** — em todos os frames |
| G3 · ritmo estável (p95 < 40 ms) | **PASSA** — p95 de 18,4 ms |
| G3 · codificar não estrangula | **PASSA** — perde 4% do ritmo |

## Duas coisas que só se souberam por medir

**A primeira medição deu 2,5 fps e não era avaria.** O Windows Graphics Capture só
entrega frames quando o ecrã muda; nessa corrida o ambiente de trabalho estava parado.
Isso não é um defeito a corrigir — é exatamente a propriedade que a camada 1 do orçamento
de upload queria: **um ecrã parado custa quase nada**, sem termos de programar nada para
isso. Só se percebeu porque a segunda versão do spike passou a correr duas passagens
sobre a mesma cena.

**Uma medição só não distingue causa de efeito.** A primeira versão media captura e
codificação juntas, e um número baixo não dizia qual das duas era a culpada — e as duas
respostas levam a sítios opostos. Por isso o spike corre agora duas passagens seguidas,
idênticas menos no codificador. A diferença entre elas *é* o custo de codificar.

E foi por isso que o G1 se mede em dois tempos. Saber que existe um NVENC registado no
Media Foundation não prova que o nosso caminho o usa — o `MediaTranscoder` podia estar a
cair no codificador de software na mesma. O que prova é o `nvidia-smi` a marcar 0% na
passagem sem codificar e 6–7% na que codifica.

## O lado de quem vê — meio respondido

A app pergunta agora à própria webview, no arranque, o que ela consegue descodificar, e
escreve a resposta. Nesta máquina:

```
capacidades: WebCodecs presente · config H.264 1080p:
             prefere-hardware=aceite prefere-software=aceite indiferente=aceite
```

Portanto o `VideoDecoder` existe na WebView2 e aceita H.264 a 1080p nas três preferências.
**O que isto não diz** — e é preciso dizê-lo, porque é fácil ler a mais: o
`isConfigSupported` responde que a *configuração* é aceite, não que a descodificação vá
parar ao hardware. O `prefer-hardware` é uma dica, e a config devolvida limita-se a
repetir a preferência que se pediu. Quem responde a "usou mesmo o hardware" é a utilização
do descodificador da GPU com um stream a sério, tal como se fez do lado do codificador.

E porque a resposta muda com a versão da WebView2 instalada e com a placa de cada um, isto
fica a correr em todas as máquinas, não só nesta.

## Transmitir, não gravar

Captar e codificar não chega: um MP4 normal só fica legível no fim, porque o índice é
escrito depois de tudo. Isso é um ficheiro, não uma transmissão. A saída passa por um
**sink de MP4 fragmentado** (`MFCreateFMPEG4MediaSink`) que escreve o cabeçalho uma vez e
a seguir despeja fragmentos independentes, e por um `IMFByteStream` implementado por nós
que, em vez de gravar, chama uma função com os bytes acabados de sair.

Escolheu-se o `IMFSinkWriter` em vez de falar diretamente com o MFT de H.264 por duas
razões concretas, que são onde se perde tempo: **ele insere sozinho a conversão de cor**
(a captura dá BGRA, os codificadores de hardware querem NV12, e fazer isso no CPU a
3440×1440 custa mais do que codificar) e **trata do MFT assíncrono da NVIDIA**, que não se
usa com um `ProcessInput`/`ProcessOutput` simples. Também faz a redução para 1920×804 —
é assim que um ecrã ultrawide cabe no upload de alguém.

### A armadilha que custou uma tarde e não dá erro nenhum

Com `BeginWrite` e `EndWrite` a devolver `E_NOTIMPL`, **o sink não falha: fica à espera
para sempre**. Sem uma linha de aviso, sem exceção, sem nada no log. De fora parecia que a
captura era lenta — foi preciso ver que o processo tinha 5,8 s de CPU parados e 503 MB para
perceber que estava bloqueado e não a trabalhar.

O sink escreve pelo caminho assíncrono, e um `IMFByteStream` que só implementa o síncrono
está incompleto de uma maneira que o compilador não apanha. Agora escreve-se logo (é para
memória, não vale a pena adiar) e avisa-se pela fila de trabalho do Media Foundation, em
vez de chamar o `Invoke` diretamente — isso reentraria no sink a meio de ele escrever.

### Como se sabe que é vídeo e não bytes com boa aparência

Contar bytes não distingue vídeo de lixo. O spike lê as caixas de topo do resultado e
exige a ordem certa: `ftyp`, depois `moov` uma vez, e a seguir **pares `moof`+`mdat`
repetidos** — e é nos pares repetidos que está a diferença entre um ficheiro e uma
transmissão. O ficheiro fica em `%TEMP%ruma-spike4.mp4` para se poder abrir e confirmar
com os olhos, que é a única verificação que nenhum teste substitui.

## O que este spike NÃO responde

- **Descodificar a sério.** Aceitar a configuração não é decodificar frames. Falta pegar
  nos NALs que este spike produz, atirá-los ao `VideoDecoder` e ver imagem.
- **O `send_frame` devolve em ~0 ms**, porque enfileira em vez de bloquear. Portanto o
  custo real de codificar não é esse número; é a diferença de ritmo entre as duas
  passagens (4%) e os 6% do NVENC.
- **Máquinas sem NVENC.** Os amigos com GPUs mais antigas ou Intel/AMD caem noutro MFT.
  A lista mostra que há alternativas de software, mas o custo delas não foi medido.
- **A latência do fragmento.** Os fragmentos saem em rajadas, com até ~330 ms entre elas.
  Para partilha de ecrã serve — o plano já tinha assumido que ~150 ms por salto era
  irrelevante aqui —, mas não serve para nada onde a resposta imediata conte. Reduzir isto
  significa fragmentos mais curtos, ou trocar o contentor por NALs crus com WebCodecs.
- **Máquinas sem NVENC.** Os amigos com GPUs mais antigas ou Intel/AMD caem noutro MFT.
  A lista mostra que há alternativas de software, mas o custo delas não foi medido.
