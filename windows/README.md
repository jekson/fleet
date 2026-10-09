# Fleet для Windows

Монитор одного сервера (Debian/Ubuntu + Docker, DigitalOcean) поверх `fleet-core-ffi`.
Tauri v2, фронтенд — чистый HTML/CSS/JS в `ui/`, Rust — `src-tauri/`.

Приложение **только читает**. Единственное изменение на сервере — `install_agent`
в режиме **Agent-only** (Fleet не трогает firewall, баны, authorized_keys, профили).

## Сборка

Нужны Rust (rust-toolchain.toml), MSVC Build Tools + Windows SDK, Node, WebView2.
Запускать из PowerShell/cmd (в Git Bash `/usr/bin/link` перекрывает MSVC `link.exe`).

```powershell
cd windows
npm install
npx tauri build      # → target\release\fleet-windows.exe
                     #   target\release\bundle\nsis\Fleet_0.1.0_x64-setup.exe
npx tauri dev        # отладка
```

Фронтенд встраивается в exe при сборке: после правок в `ui/` нужна пересборка.

## Данные и ключи

Где лежат (первое подходящее):
1. `FLEET_WIN_DIR` → `<dir>\data\`, `<dir>\config.local.toml`;
2. exe запущен из `target\` (сборка из исходников) → `windows\data\`, `windows\config.local.toml`;
3. иначе (установленное приложение) → всё в одной папке `%LOCALAPPDATA%\Fleet\`.

Содержимое: `keys\*.key` — программные P-256 ключи (root, device, monitor, ssh,
monitor_ssh), Noise-ключ и ключ целостности кэша (hex, без шифрования); `cache.sqlite` —
серверы и пины; `recovery.txt` — 24 слова восстановления. Путь и кнопка «Open folder» —
в Settings. **Папка = root-доступ к серверу**: передавать только по защищённому каналу.
`windows/data/` и `config.local.toml` в `.gitignore`.

Первый запуск сам создаёт fleet (genesis roster, этот ПК — единственное устройство),
занимает ~10–30 с (Argon2).

## Конфиг

`config.local.toml` (шаблон — `config.example.toml`). Всё это же редактируется в
Settings (Save перезаписывает файл). DO-токен достаточно read-only; кнопка
«Загрузить список из аккаунта» подставит droplet id / database id.

## Добавить сервер

Мастер Add server, способ по умолчанию — **«через мой SSH-ключ»**: укажите IP/порт/user и
(необязательно) путь к своему ключу; пусто = `~/.ssh/id_*` и ssh-agent. Приложение
запускает Windows OpenSSH (`ssh.exe`, BatchMode), дописывает свой ключ в
`~/.ssh/authorized_keys`, читает отпечатки host key по этому же (проверенному вашим
known_hosts) каналу, само подтверждает совпавший отпечаток и ставит агент (Agent-only).
Требования: хост уже есть в вашем known_hosts (подключитесь раз из терминала), ключ без
пароля или загружен в ssh-agent.

Альтернативы: скопировать ключ/команду вручную; одноразовый пароль (не сохраняется).
`.deb` агента по умолчанию `target/linux/x86_64/fleet-agent_*_amd64.deb`. Если агент
уже стоит — Adopt / Replace.

## Экраны

Fleet (сводка, CPU/RAM/диск), сервер: Overview (live-метрики 1 с, история за час с
момента запуска), Docker (контейнеры, только чтение), Logs (journal: запрос и follow),
DigitalOcean (droplet + managed DB, работает и без агента), Alerts, Settings.

## Алерты

Каждые 15 с: CPU > X% дольше N мин, RAM > X%, диск / > X%, агент недоступен > N мин,
droplet не `active`, БД не `online`, CPU/диск БД выше порога, алерты самого агента.
Метрики дроплета из DO используются, когда агент молчит. Windows-toast при
срабатывании, повтор не чаще cooldown. Иконка в трее: зелёная/оранжевая/красная;
закрытие окна прячет его в трей, выход — через меню трея.

## Второй разработчик

Оба работают под одной идентичностью (одни ключи), оба видят сервер и получают алерты;
одновременная работа проверена.

1. Установить `Fleet_0.1.0_x64-setup.exe` и один раз закрыть приложение (выход из трея).
2. Первый разработчик, **закрыв приложение**, передаёт по защищённому каналу содержимое
   своей папки данных: `windows\data\*` (+ `windows\config.local.toml`) или
   `%LOCALAPPDATA%\Fleet\*`.
3. Второй кладёт это в `%LOCALAPPDATA%\Fleet\` (заменив созданное при первом запуске:
   `keys\`, `cache.sqlite*`, `recovery.txt`, `config.local.toml`) и запускает приложение.
