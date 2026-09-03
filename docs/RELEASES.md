# Publicar o Bruma — a chave, os portões, e o que fazer quando corre mal

> Escrito a 30/08/2026 (v0.18.3). Cada secção tem a data em que foi verificada pela última
> vez. Um documento destes envelhece e mente; quando seguires uma receita e ela não bater
> certo com a realidade, corrige o documento no mesmo commit em que corriges o problema.

## A chave de assinatura — a única raiz de confiança *(30/08/2026)*

O auto-update só instala o que verificar com a chave minisign. Não há revogação: quem tiver
a chave privada assina uma actualização que todas as máquinas instaladas correm sozinhas.
É a coisa mais valiosa do projecto.

**Onde vive:**

| O quê | Onde |
|---|---|
| Chave privada (cifrada com password) | segredo `TAURI_SIGNING_PRIVATE_KEY` do repositório GitHub |
| Password da chave | segredo `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` |
| A mesma chave, na máquina do dono | `~/.tauri/` |
| Chave **pública** | `apps/desktop/tauri.conf.json` → `plugins.updater.pubkey` (vai dentro da app) |

**A cópia de segurança — e a prova de que serve.** Uma cópia que nunca foi testada é uma
cópia que não existe. Guarda a chave privada e a password fora do GitHub e fora da máquina
(gestor de palavras-passe, ou papel num sítio físico diferente). Para provar que a cópia
serve:

```bash
echo teste > rascunho.bin
TAURI_SIGNING_PRIVATE_KEY="$(cat copia-da-chave)" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="..." \
  npx --yes "@tauri-apps/cli@2.11.4" signer sign rascunho.bin
cargo run -q -p verificar-assinatura -- rascunho.bin rascunho.bin.sig apps/desktop/tauri.conf.json
rm rascunho.bin rascunho.bin.sig
```

Se a última linha disser «assinatura confere», a cópia serve. O workflow da release corre
exactamente esta prova em cada publicação (passo «A chave privada e a publica sao mesmo
par?»), portanto uma dessincronização nunca chega a custar uma compilação inteira.

**Rodar a chave.** As duas metades rodam **juntas, na mesma release**: gera-se o par novo
(`npx @tauri-apps/cli signer generate`), troca-se a `pubkey` no `tauri.conf.json` **e** os
dois segredos no GitHub, e publica-se. Quem estiver numa versão com a pubkey antiga só se
actualiza para releases assinadas com a chave antiga — por isso, se a chave privada antiga
ainda existir, assina-se com ela **uma última versão** cujo único conteúdo é a pubkey nova;
quem a instalar volta a ter actualizações. Se a chave antiga se perdeu de vez, as
instalações existentes têm de descarregar o instalador novo à mão — e é para isso que o
aviso de versão no protocolo (`Ola.versao`) existe: os pares dizem uns aos outros que há
coisa mais nova.

## A v0.23.0 muda o significado de `ApagarCanal` *(03/09/2026)*

A Fase 8 transformou «apagar um canal» em «arquivar»: o canal sai da barra, o log fica, e um
`CriarCanal` posterior com o MESMO id reabre-o. As duas versões reconstroem listas
DIFERENTES do mesmo log:

| Do mesmo log | ≤ v0.22.0 vê | ≥ v0.23.0 vê |
|---|---|---|
| Criar c1, Apagar c1 | sem c1 | c1 em «Arquivados», a ler-se |
| Criar c1, Apagar c1, Criar c1 | **sem c1** | c1 aberto outra vez |

Consequência prática: **as duas máquinas têm de estar na v0.23.0 antes de alguém arquivar ou
reabrir um canal.** Uma instalação antiga não vê o canal reaberto — e as mensagens escritas lá
depois não lhe aparecem em sítio nenhum, sem um aviso. O `peer-versao` já mostra a versão do
outro lado na lista de membros; é por lá que se confirma antes de mexer.

Não há migração e não é preciso: o log não muda, só a leitura dele.

## Retirar uma versão má *(30/08/2026)*

```bash
gh release edit vX.Y.Z --draft
```

Um rascunho desaparece do `releases/latest`: o updater de toda a gente volta a ver a versão
anterior como a mais recente, e quem ainda não actualizou já não actualiza para a má. Quem
JÁ instalou fica nela até haver uma mais nova — publica-se a correcção como versão seguinte,
nunca reescrevendo a etiqueta má. **Nunca apagar a etiqueta git**: o histórico de que ela
existiu é parte da honestidade do projecto.

## A prova de que uma instalação N-1 se actualiza para N *(30/08/2026)*

Os portões da release provam que o que foi publicado está coerente. O workflow
**Prova de actualizacao** (`workflow_dispatch`) prova a outra metade: instala o instalador
*realmente publicado* da versão anterior, põe-lhe dados dentro, actualiza por cima com o
dialecto do updater (`/P /UPDATE`), e exige que a app passe a dizer a versão nova, que os
dados sobrevivam byte a byte, e que o desinstalador tenha sido substituído. Correr depois
de cada release custa um clique (Actions → Prova de actualizacao → as duas etiquetas).

## Os alvos fixos do workflow *(30/08/2026)*

As acções do `release.yml` estão fixadas por **SHA de commit** (o número legível fica em
comentário) e o CLI do Tauri por versão exacta, porque esses passos correm com a chave no
ambiente e `@v0`/`^2` são alvos móveis. Subir de versão é uma decisão com commit:

```bash
gh api repos/<dono>/<repo>/git/ref/tags/<tag> -q '.object.sha'
```

— e se o tipo devolvido for `tag` (anotada), resolve-se mais um nível com
`gh api repos/<dono>/<repo>/git/tags/<sha> -q '.object.sha'`.

## A herança NSIS *(30/08/2026)*

Até à v0.10.x o que se descarregava era o instalador NSIS do Tauri. Quem instalou por ele
tem no disco um desinstalador que, ao perguntar «apagar também os dados da aplicação?»,
limpa `%APPDATA%\dev.bruma.app` — uma pasta onde o Bruma **nunca guardou nada**. Os dados
vivem em `%APPDATA%\Bruma`. Ou seja: nessas instalações antigas, a promessa de apagar a
identidade não se cumpre, e não há forma de corrigir retroactivamente um desinstalador que
já está nas máquinas.

O desinstalador actual (o nosso) olha para os sítios certos — `%APPDATA%\Bruma`, a pasta
`dados` ao lado do exe, e o `BRUMA_DADOS` se estiver definido — e **diz quantas pastas
apagou mesmo**, em vez de garantir sem olhar. Quem removeu uma instalação NSIS antiga e
quer mesmo perder a identidade apaga `%APPDATA%\Bruma` à mão.

## O mapa dos portões *(30/08/2026)*

A razão de cada um está em comentário no próprio `release.yml`, ao lado do passo. Por
ordem: a prova do par de chaves; a versão igual nos quatro sítios (etiqueta,
`tauri.conf.json` da app, `Cargo.toml`, `tauri.conf.json` do instalador); fmt, clippy e
testes; os nomes `BRUMA_*` das fontes todos classificados; a release nasce rascunho; o
binário sem andaimes (e o portão a provar que ainda sabe reprovar); o instalador executado
— instala, actualiza por `/P /UPDATE`, relança com `/ARGS`, desinstala com e sem `--dir`,
sem tocar no que é do utilizador; o instalador sem andaimes; a assinatura verificada com a
chave que vai dentro da app (e a prova de que um byte trocado é recusado); e só então a
publicação, com as notas da etiqueta na página e no `latest.json`.
