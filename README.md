<div align="center">

# 🧹 kika

**Recupere gigabytes de disco sem medo de apagar o que não devia.**

O kika varre seus projetos atrás de `node_modules`, builds, `target/` do Rust e arquivos temporários,<br>
soma o que os caches do Docker, pnpm, uv, yarn e npm estão ocupando e deixa você escolher o que limpar.

![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white)
![Versão](https://img.shields.io/badge/vers%C3%A3o-0.4.0-blue)
![Plataforma](https://img.shields.io/badge/plataforma-Linux%20%C2%B7%20macOS%20%C2%B7%20WSL-lightgrey)

<img src="assets/demo.gif" alt="kika listando projetos e caches, marcando dois repositórios e limpando só eles" width="860">

</div>

---

## ✨ Destaques

- 🔍 **Relatório primeiro, sempre.** Rodar `kika` nunca apaga nada: ele mostra o que encontrou e quanto espaço dá para liberar
- 🗂️ **Lista interativa.** Os achados são agrupados por repositório git; marque os que quiser e limpe só eles, ou tudo de uma vez
- 🌍 **Caches globais inclusos.** Docker build cache, pnpm store, uv, yarn e npm entram no mesmo relatório
- 🦀 **Rust do jeito certo.** `target/` só é tratado ao lado de um `Cargo.toml` e é limpo com `cargo clean --offline`
- 🟢 **Entende fnm e nvm.** Encontra `npm`, `pnpm` e `yarn` mesmo fora de um shell inicializado (cron, scripts)
- ⚡ **Cara de CLI moderna.** Spinner com progresso no stderr, saída limpa em pipes e respeito ao `NO_COLOR`

## 📦 Instalação

Requer [Rust](https://rustup.rs) (edição 2024).

```bash
cargo install --git https://github.com/juniormartinxo/kika
```

Ou a partir de um clone local:

```bash
git clone https://github.com/juniormartinxo/kika
cd kika
cargo install --path .
```

## 🚀 Uso rápido

```bash
kika                 # relatório do diretório atual + caches globais (não apaga nada)
kika ~/projetos      # o mesmo, para outro diretório
kika clean           # remove tudo o que o relatório lista, após confirmação
kika clean --yes     # remove sem perguntar (ideal para automação)
```

| Comando                                                  | O que faz                                                      |
| -------------------------------------------------------- | -------------------------------------------------------------- |
| `kika [path]`                                            | Relatório completo; no terminal abre a lista interativa        |
| `kika report [path] --plain`                             | Relatório em texto, sem interação                              |
| `kika report [path] --output relatorio.txt`              | Salva o relatório em arquivo                                   |
| `kika clean [path]`                                      | Pede confirmação e remove tudo, incluindo os caches globais    |
| `kika clean [path] --yes`                                | Remove sem prompt                                              |
| `kika clean [path] --no-caches`                          | Limpa só o diretório, sem tocar nos caches globais             |
| `kika clean --docker-builder --pnpm-store`               | Limpa o diretório e apenas os caches escolhidos                |
| `kika report [path] --include-old-files --older-than-days 90` | Inclui arquivos com mais de 90 dias (com `clean`, são removidos) |

Rode `kika --help` para a lista completa de opções.

## 🖥️ Lista interativa

No terminal, `kika` abre uma lista com tudo o que foi encontrado:

```
┌ kika · 10 groups · 17.9 GB reclaimable ──────────────────────────────────┐
│      Name                      Items      Size Contents                  │
│› [x] storefront                    3    1.1 GB node_modules, .next       │
│  [ ] design-system                 3  467.0 MB node_modules, dist, temp… │
│  [x] sketches                      1   64.0 MB node_modules              │
│      Global caches                                                       │
│  [ ] uv cache                      1    7.1 GB uv-cache                  │
│  [ ] Docker build cache            1    2.9 GB docker-cache              │
└──────────────────────────────────────────────────────────────────────────┘
 Clean selected · 2 · 1.1 GB    Clean all · 17.9 GB
```

| Tecla                   | Ação                                       |
| ----------------------- | ------------------------------------------ |
| `↑` `↓` / `j` `k`       | Navegar                                    |
| `PgUp` `PgDn`           | Pular uma página                           |
| `Home` `End` / `g` `G`  | Ir ao início ou ao fim                     |
| `espaço` ou clique      | Marcar/desmarcar o grupo                   |
| `a`                     | Marcar/desmarcar tudo                      |
| `Tab`                   | Alternar o foco entre lista e botões       |
| `Enter`                 | Marcar ou acionar o botão focado           |
| `c`                     | Limpar os selecionados                     |
| `C`                     | Limpar tudo                                |
| `q` / `Esc` / `Ctrl+C`  | Sair sem apagar nada                       |

- Os itens são agrupados pelo repositório git mais próximo, sem subir acima do diretório escaneado; fora de um repositório, pelo diretório do projeto. Cada cache global vira uma linha própria
- Antes de limpar, uma confirmação mostra quantos grupos, itens e bytes serão removidos, e só `y` confirma
- Teclas digitadas durante o scan são descartadas, para não acionarem nada sem querer
- Com pipe, `--output` ou `--plain`, sai o relatório em texto, sem interação

## 🔎 O que o kika encontra

**No diretório escaneado**

| Alvo                         | Detalhes                                                                         |
| ---------------------------- | -------------------------------------------------------------------------------- |
| `node_modules`               | Dependências de projetos JavaScript/TypeScript                                   |
| `dist`, `.next`              | Saídas de build                                                                  |
| `target`                     | Só em projetos Rust (com `Cargo.toml` irmão), limpo via `cargo clean --offline`  |
| Arquivos temporários         | `.tmp`, `.temp`, `.bak`, `.old`, `.orig`, `.rej`, `.swp` e `.swo`                |
| `Zone.Identifier`            | Alternate data streams criados pelo Windows/WSL                                  |
| Arquivos antigos             | Só com `--include-old-files` e `--older-than-days`                               |

**Caches globais** (fora do diretório escaneado, todos incluídos por padrão)

| Flag               | Cache                               | Comando executado no `clean`   |
| ------------------ | ----------------------------------- | ------------------------------ |
| `--docker-builder` | Build cache do Docker               | `docker builder prune --force` |
| `--pnpm-store`     | Pacotes não referenciados do pnpm   | `pnpm store prune`             |
| `--uv-cache`       | Cache do uv                         | `uv cache clean`               |
| `--yarn-cache`     | Cache do yarn                       | `yarn cache clean`             |
| `--npm-cache`      | Cache do npm                        | `npm cache clean --force`      |

Sem flags de cache, todos entram. Com uma ou mais flags, só os escolhidos entram; `--no-caches` deixa os caches globais de fora.

<details>
<summary><b>Como os caches são medidos e encontrados</b></summary>

- O tamanho no relatório é uma estimativa: para o Docker vem do `Reclaimable` de `docker system df`; para os demais, é o tamanho do diretório de cache (no pnpm é um teto, já que `pnpm store prune` mantém pacotes ainda referenciados)
- No `clean`, o espaço informado é o realmente liberado, medido de novo após a limpeza ou lido da saída do Docker
- Ferramenta ausente ou com erro gera um aviso e o cache é ignorado, sem interromper o restante
- `npm`, `pnpm` e `yarn` são procurados primeiro no `PATH`. Se não estiverem lá, o kika usa o Node do fnm (`$FNM_DIR`, `~/.local/share/fnm`, `~/.fnm`) ou do nvm (`$NVM_DIR`, `~/.nvm`), preferindo o alias `default` e depois a versão mais recente instalada. O `bin` dessa versão vai para o `PATH` do processo filho, para que o `node` seja encontrado

</details>

## 🛡️ Segurança

- O comando padrão é o relatório, que nunca apaga nada
- Remoção real só acontece com `kika clean` ou pela lista interativa, sempre com confirmação (a menos que você passe `--yes`)
- Limpeza de arquivos antigos exige `--include-old-files` e `--older-than-days`
- O scanner ignora links simbólicos e `.git`
- Diretórios `target` só são tratados em projetos Rust; os demais `target/` são ignorados
- Sem `cargo` no `PATH`, o kika reporta o `target/`, mas a limpeza falha com aviso, sem remoção forçada
- O antigo `--apply` ainda é aceito no `clean`, mas é redundante

## 🎬 Regravando o GIF

O GIF acima é gerado com [VHS](https://github.com/charmbracelet/vhs) a partir de [`demo/demo.tape`](demo/demo.tape). O script [`demo/setup.sh`](demo/setup.sh) monta um workspace descartável em um diretório temporário, com arquivos esparsos e ferramentas de cache falsas no `PATH`, então gravar nunca toca nos seus caches de verdade:

```bash
vhs demo/demo.tape   # gera assets/demo.gif
```
