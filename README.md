# kika

CLI em Rust para localizar e remover lixo comum de projetos frontend e diretórios de trabalho.

Escopo inicial:

- `node_modules`
- `dist`
- `.next`
- arquivos temporários como `.tmp`, `.temp`, `.bak`, `.old`, `.orig`, `.rej`, `.swp` e `.swo`
- arquivos `Zone.Identifier` (alternate data streams criados pelo Windows/WSL)
- arquivos antigos, mas apenas em modo explícito com cutoff em dias

## Uso

`cargo run -- [path]`

Executa um `dry-run` e mostra o que seria removido.

`cargo run -- clean [path] --apply`

Pede confirmação e, se você confirmar, remove de fato os diretórios e arquivos encontrados.

`cargo run -- clean [path] --apply --yes`

Remove sem prompt de confirmação. Esse modo é o mais adequado para automação.

`cargo run -- report [path] --output cleanup-report.txt`

Gera um relatório e salva em arquivo, sem apagar nada.

`cargo run -- report [path] --include-old-files --older-than-days 90`

Gera um relatório incluindo arquivos regulares mais antigos que 90 dias.

`cargo run -- clean [path] --apply --include-old-files --older-than-days 90`

Além das regras padrão, remove arquivos regulares mais antigos que 90 dias.

## Segurança

- O modo padrão é `dry-run`
- Remoção real só acontece com `clean --apply`
- `clean --apply` pede confirmação, a menos que você passe `--yes`
- Limpeza de arquivos antigos exige `--include-old-files` e `--older-than-days`
- O scanner ignora links simbólicos, `.git` e `target`
