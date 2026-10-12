# kika

CLI em Rust para localizar e remover lixo comum de projetos frontend e diretórios de trabalho.

Escopo inicial:

- `node_modules`
- `dist`
- `.next`
- `target` de projetos Rust (limpeza via `cargo clean --offline`, apenas quando há `Cargo.toml` irmão e `cargo` no `PATH`)
- arquivos temporários como `.tmp`, `.temp`, `.bak`, `.old`, `.orig`, `.rej`, `.swp` e `.swo`
- arquivos `Zone.Identifier` (alternate data streams criados pelo Windows/WSL)
- arquivos antigos, mas apenas em modo explícito com cutoff em dias

Caches globais de ferramentas (fora do diretório escaneado, todos incluídos por padrão):

| Flag               | Comando executado no `clean`   |
| ------------------ | ------------------------------ |
| `--docker-builder` | `docker builder prune --force` |
| `--pnpm-store`     | `pnpm store prune`             |
| `--uv-cache`       | `uv cache clean`               |
| `--yarn-cache`     | `yarn cache clean`             |
| `--npm-cache`      | `npm cache clean --force`      |

Sem flags de cache, todos entram. Passando uma ou mais flags, só os escolhidos entram; `--no-caches` deixa os caches globais de fora.

## Uso

Enquanto verifica e limpa, o kika mostra no stderr um spinner com o progresso (entradas lidas, matches e caminho atual) e termina com uma linha `✔` com o tempo gasto. A animação só aparece quando o stderr é um terminal (pipes, arquivos e cron recebem saída limpa) e as cores são desativadas com `NO_COLOR`.

`kika [path]`

Relatório completo do diretório (padrão: o atual) e de todos os caches globais. Não apaga nada.

`kika clean [path]`

Pede confirmação e remove tudo o que o relatório lista, incluindo os caches globais.

`kika clean [path] --yes`

Remove sem prompt de confirmação. Esse modo é o mais adequado para automação.

`kika clean [path] --no-caches`

Limpa só o diretório, sem tocar nos caches globais.

`kika clean --docker-builder --pnpm-store`

Limpa o diretório e apenas o build cache do Docker e o store do pnpm.

`kika report [path] --output cleanup-report.txt`

Gera o relatório e salva em arquivo.

`kika report [path] --include-old-files --older-than-days 90`

Inclui no relatório arquivos regulares mais antigos que 90 dias (com `clean`, eles são removidos).

### Caches globais

- O tamanho no relatório é uma estimativa: para Docker vem do `Reclaimable` de `docker system df`; para os demais é o tamanho do diretório de cache (no pnpm é um teto, já que `pnpm store prune` mantém pacotes ainda referenciados)
- No `clean`, o espaço informado é o realmente liberado (medido de novo após a limpeza ou lido da saída do Docker)
- Ferramenta ausente ou com erro gera um aviso e o cache é ignorado, sem interromper o restante
- `npm`, `pnpm` e `yarn` são procurados primeiro no `PATH`; se não estiverem lá (cron, shell não interativo), o kika usa o Node do fnm (`$FNM_DIR`, `~/.local/share/fnm`, `~/.fnm`) ou do nvm (`$NVM_DIR`, `~/.nvm`), preferindo o alias `default` e depois a versão mais recente instalada. O `bin` dessa versão é colocado no `PATH` do processo filho para que o `node` seja encontrado

## Segurança

- O comando padrão é o relatório, que nunca apaga nada
- Remoção real só acontece com `kika clean` (o antigo `--apply` ainda é aceito, mas é redundante)
- `kika clean` pede confirmação, a menos que você passe `--yes`
- Limpeza de arquivos antigos exige `--include-old-files` e `--older-than-days`
- O scanner ignora links simbólicos e `.git`
- Diretórios `target` só são tratados em projetos Rust (com `Cargo.toml` irmão); demais `target/` são ignorados
- Sem `cargo` no `PATH`, o kika reporta o `target/` mas falha o apply com aviso, sem remoção forçada
