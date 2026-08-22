; Textos do instalador do Bruma, na voz da app.
;
; Isto substitui as strings PT-BR que o Tauri traz de fábrica. Duas razões para existir:
; as originais têm erros ("primeirl", "executare") e um tom genérico que não é o nosso —
; e há uma que precisa mesmo de ser outra: o "Remover dados do programa" do desinstalador.
; No Bruma, os dados SÃO a identidade. Não há conta, não há servidor, não há recuperação:
; quem apaga a pasta de dados apaga a conta para sempre, e a caixa tem de o dizer.

LangString addOrReinstall ${LANG_PORTUGUESEBR} "Adicionar ou reinstalar componentes"
LangString alreadyInstalled ${LANG_PORTUGUESEBR} "Já instalado"
LangString alreadyInstalledLong ${LANG_PORTUGUESEBR} "O ${PRODUCTNAME} ${VERSION} já está instalado. Escolhe o que fazer e carrega em Próximo."
LangString appRunning ${LANG_PORTUGUESEBR} "O {{product_name}} está aberto. Fecha a janela dele e tenta outra vez."
LangString appRunningOkKill ${LANG_PORTUGUESEBR} "O {{product_name}} está aberto.$\nCarrega em OK para o fechar."
LangString chooseMaintenanceOption ${LANG_PORTUGUESEBR} "Escolhe a operação de manutenção."
LangString choowHowToInstall ${LANG_PORTUGUESEBR} "Escolhe como queres instalar o ${PRODUCTNAME}."
LangString createDesktop ${LANG_PORTUGUESEBR} "Criar atalho na área de trabalho"
LangString dontUninstall ${LANG_PORTUGUESEBR} "Não desinstalar"
LangString dontUninstallDowngrade ${LANG_PORTUGUESEBR} "Não desinstalar (voltar a uma versão anterior sem desinstalar está desativado neste instalador)"
LangString failedToKillApp ${LANG_PORTUGUESEBR} "Não consegui fechar o {{product_name}}. Fecha a janela dele primeiro e tenta outra vez."
LangString installingWebview2 ${LANG_PORTUGUESEBR} "A instalar o WebView2 (o motor que desenha a interface)…"
LangString newerVersionInstalled ${LANG_PORTUGUESEBR} "Já tens uma versão MAIS RECENTE do ${PRODUCTNAME} instalada. Voltar atrás não é recomendado — as versões novas falam um protocolo que as antigas não entendem. Se quiseres mesmo, desinstala a atual primeiro. Escolhe o que fazer e carrega em Próximo."
LangString older ${LANG_PORTUGUESEBR} "mais antiga"
LangString olderOrUnknownVersionInstalled ${LANG_PORTUGUESEBR} "Está instalada uma versão $R4 do ${PRODUCTNAME}. O instalador remove-a primeiro e mantém os teus dados — a identidade e as mensagens ficam onde estão. Escolhe o que fazer e carrega em Próximo."
LangString silentDowngrades ${LANG_PORTUGUESEBR} "Este instalador não permite voltar a versões anteriores em modo silencioso. Usa a interface gráfica.$\n"
LangString unableToUninstall ${LANG_PORTUGUESEBR} "Não foi possível desinstalar a versão anterior."
LangString uninstallApp ${LANG_PORTUGUESEBR} "Desinstalar o ${PRODUCTNAME}"
LangString uninstallBeforeInstalling ${LANG_PORTUGUESEBR} "Desinstalar antes de instalar"
LangString unknown ${LANG_PORTUGUESEBR} "desconhecida"
LangString webview2AbortError ${LANG_PORTUGUESEBR} "A instalação do WebView2 falhou, e o ${PRODUCTNAME} não corre sem ele. Reinicia o instalador para tentar de novo."
LangString webview2DownloadError ${LANG_PORTUGUESEBR} "Erro ao descarregar o WebView2 — $0"
LangString webview2DownloadSuccess ${LANG_PORTUGUESEBR} "WebView2 descarregado"
LangString webview2Downloading ${LANG_PORTUGUESEBR} "A descarregar o WebView2…"
LangString webview2InstallError ${LANG_PORTUGUESEBR} "A instalação do WebView2 falhou com o código $1"
LangString webview2InstallSuccess ${LANG_PORTUGUESEBR} "WebView2 instalado"
LangString deleteAppData ${LANG_PORTUGUESEBR} "Apagar também a identidade e as mensagens — ATENÇÃO: no Bruma não há conta nem servidor; a identidade vive só neste computador e apagá-la é perdê-la PARA SEMPRE. Ninguém ta consegue devolver."
