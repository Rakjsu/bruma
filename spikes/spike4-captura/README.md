# Spike 4 — captura nativa, sem passar pelo `getDisplayMedia`

**Estado: PASSA.** Os quatro gates passaram, e o mais importante — o codificador por
hardware — passou com prova direta, não por inferência.

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
que a WebView2 tem e acelera por hardware.

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

## O que este spike NÃO responde

- **O lado de quem vê.** O `VideoDecoder` do WebCodecs dentro da WebView2 ainda não foi
  testado. Não se testa a descodificação antes de haver alguma coisa medida para
  descodificar — é o passo seguinte, e é o próximo gate a sério.
- **O `send_frame` devolve em ~0 ms**, porque enfileira em vez de bloquear. Portanto o
  custo real de codificar não é esse número; é a diferença de ritmo entre as duas
  passagens (4%) e os 6% do NVENC.
- **Máquinas sem NVENC.** Os amigos com GPUs mais antigas ou Intel/AMD caem noutro MFT.
  A lista mostra que há alternativas de software, mas o custo delas não foi medido.
- **Elementary stream.** O `VideoSettingsSubType` tem `H264ES` (NALs crus, sem
  contentor), que é o que queremos mandar pelo iroh. Esta medição usou o H.264 dentro de
  MP4 em memória, porque o objetivo era o ritmo e o débito, não o formato de saída.
