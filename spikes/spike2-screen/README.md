# Spike 2 — partilha de ecrã dentro do Tauri

Responde a duas perguntas que o plano deixou em aberto e que ninguém documenta em lado nenhum:

1. **O `getDisplayMedia()` funciona dentro da webview do Tauri no Windows?**
   Concretamente: aparece um picker para escolher a fonte, vem áudio do sistema, e vem o cursor?
2. **Quanto custa mesmo em upload?**
   Qual o bitrate real por codec e por `contentHint`, e se o AV1 vai por **hardware** ou por software.

A segunda pergunta mede-se sem rede nenhuma: duas `RTCPeerConnection` na mesma máquina, ligadas uma
à outra. O encoder é o mesmo que seria usado numa chamada real, portanto os números são reais.

## Resultado — 21/08/2026 (RTX 5080, WebView2 151, Windows 11)

**O gate passa.** A partilha de ecrã funciona dentro do Tauri. Mas duas das ressalvas que o plano
listava como hipóteses confirmaram-se, e mudam a estratégia de upload.

### O que funciona, e melhor do que se esperava

| | Resultado |
|---|---|
| `getDisplayMedia()` na webview | **funciona** |
| Picker de fontes | **aparece** — separadores *Janela* e *Tela Inteira*, com miniaturas |
| Resolução capturada | **3840×2160** |
| Framerate pedido | 60 |
| `displaySurface` | `monitor` |
| `cursor` | `always` — o cursor vem incluído |
| Codecs disponíveis | VP8, H264, **AV1**, VP9 |

O picker é o do Chromium, com o título `Escolha o que compartilhar com http://tauri.localhost`. Ou
seja, o utilizador escolhe mesmo o que partilha — não há concessão silenciosa.

### O que não confirma o plano

**1. O AV1 vai por software.** O `encoderImplementation` diz `libaom` — não toca no NVENC da RTX
5080. Nesta máquina (32 núcleos) aguenta-se, mas **um amigo com CPU modesta não vai conseguir
partilhar 1080p60**, e é para os amigos que isto existe.

**2. O `contentHint` não fez diferença mensurável.**

| codec | `contentHint` | kbps | fps | encoder |
|---|---|---|---|---|
| AV1 | `text` | **2504** | 32,1 | libaom |
| AV1 | *(nenhum)* | **2489** | 32 | libaom |

Isso é 0,6% de diferença — ruído. O plano contava com 25%+ de poupança vinda das ferramentas de
*screen content coding* do AV1 (palette mode, intra block copy), e **essa poupança não apareceu**.

**3. O encoder já estava a lutar.** Pediu-se 60 fps e entregou ~32, com a resolução reduzida para
1440p de altura, sem que o `qualityLimitationReason` acusasse nada.

### Ressalva honesta sobre esta medição

O ecrã partilhado continha a própria janela da app, **incluindo a pré-visualização da captura** —
o que cria um efeito de espelho com movimento constante numa região. Não é um cenário *parado*
puro, e o `contentHint` atua melhor precisamente em conteúdo estático.

**Antes de dar o ponto 2 por fechado, vale a pena repetir com um ecrã genuinamente imóvel** —
outro monitor, com um editor de código aberto e sem a app à vista.

### O que isto muda

A camada 1 do orçamento de upload do plano — a que dependia do AV1 com `contentHint` — perde o seu
principal argumento até prova em contrário. Em compensação, **o caminho nativo em Rust ganhou muito
peso**: `scap` com os *dirty rects* do DXGI e o NVENC por hardware atacam exatamente os dois
problemas que a webview não resolve.

Em termos absolutos o número não é mau: **2,5 Mbps para um ecrã 4K** é comportável, e para três
espectadores em mesh dá ~7,5 Mbps, que cabe num upload de cabo residencial.

## Correr

```bash
cargo run -p spike2-screen
```

Abre uma janela com quatro secções. Faz por ordem:

1. **Captura** — carrega em *Testar partilha de ecrã*. Repara se aparece um picker de fontes.
   Depois repete com *Testar com áudio do sistema*.
2. **Codecs** — lista o que este encoder oferece. Confirma se há `video/AV1`.
3. **Bitrate** — escolhe o cenário (parado ou com movimento) e mede.
4. **Relatório** — copia o JSON e guarda-o.

## Como ler os resultados

**Apareceu picker?** A página mede quanto tempo o `getDisplayMedia()` demorou a resolver. Acima de
um segundo houve interação humana, ou seja, apareceu uma janela de escolha. Abaixo disso, ou foi
concedido em silêncio (mau: o utilizador não escolhe o que partilha) ou falhou.

**`encoderImplementation` é o campo que interessa** na tabela de bitrate:

| Valor | Significa |
|---|---|
| `libaom`, `libvpx`, `openh264` | **software** — CPU a arder a 1080p60 |
| qualquer outra coisa (ex.: nomes de fornecedor) | **hardware** — o encoder da GPU |

Se o AV1 aparecer como `libaom`, o caminho da webview não chega ao NVENC. Isso não é fatal, mas é
um argumento forte a favor do caminho nativo em Rust (`scap` + crate `livekit`), porque um amigo com
CPU mais fraca não vai aguentar partilhar ecrã.

**`contentHint`** deve fazer diferença visível no cenário *parado*. É ele que liga as ferramentas de
screen content coding do AV1 (palette mode, intra block copy). Se `text` e `(nenhum)` derem o mesmo
bitrate com o ecrã quieto, o hint não está a ser respeitado — e metade do orçamento de upload do
plano deixa de existir.

**`limitadoPor`** (`qualityLimitationReason`) diz porque é que o encoder baixou a qualidade: `cpu`
significa que a máquina não aguenta, `bandwidth` que o estimador de largura de banda cortou.

## Fazer as medições honestamente

O bitrate depende inteiramente do que está no ecrã. Para os números servirem para alguma coisa:

- **Cenário parado**: deixa um editor de código ou uma página de texto à frente e **não mexas no
  rato** durante a medição. É o caso normal de quem partilha ecrã para mostrar código.
- **Cenário com movimento**: põe um vídeo a correr em ecrã inteiro. É o pior caso.

Mede os dois. A diferença entre eles é o argumento inteiro a favor da codificação por regiões sujas.

## O que este spike NÃO é

- Não envia nada pela rede. O loopback é local de propósito, para isolar o custo do encoder da
  variabilidade da ligação.
- Não usa LiveKit nem E2EE. Isso entra depois, e cifrar frames não muda o bitrate do encoder.
- Não testa macOS nem Linux. O WKWebView e o WebKitGTK têm histórias diferentes e piores aqui.

Código descartável. O que sobrevive são os números e a decisão entre webview e captura nativa.
