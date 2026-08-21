# Spike 2 — partilha de ecrã dentro do Tauri

Responde a duas perguntas que o plano deixou em aberto e que ninguém documenta em lado nenhum:

1. **O `getDisplayMedia()` funciona dentro da webview do Tauri no Windows?**
   Concretamente: aparece um picker para escolher a fonte, vem áudio do sistema, e vem o cursor?
2. **Quanto custa mesmo em upload?**
   Qual o bitrate real por codec e por `contentHint`, e se o AV1 vai por **hardware** ou por software.

A segunda pergunta mede-se sem rede nenhuma: duas `RTCPeerConnection` na mesma máquina, ligadas uma
à outra. O encoder é o mesmo que seria usado numa chamada real, portanto os números são reais.

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
