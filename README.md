# wordropbot

Telegram-бот для запоминания слов: участники коллекции загружают слова с переводами, бот по расписанию присылает квизы.

## Конфигурация

Обязателен только `telegram.token`. Минимальный конфиг:

```toml
[telegram]
token = "123456:ABC..."
```

Остальные ключи необязательны, их значения по умолчанию перечислены в `deploy/config.example.toml`. Токен выдаёт @BotFather командой `/newbot`.

## Сборка

```sh
cargo build --release
```

Бинарник: `target/release/wordropbot`.

## Установка на сервер (systemd)

```sh
# бинарник
sudo install -m 755 target/release/wordropbot /usr/local/bin/wordropbot

# системный пользователь
sudo useradd --system --no-create-home --shell /usr/sbin/nologin wordropbot

# конфиг с токеном
sudo install -d -o root -g wordropbot -m 750 /etc/wordropbot
sudo install -o root -g wordropbot -m 640 deploy/config.example.toml /etc/wordropbot/config.toml
sudoedit /etc/wordropbot/config.toml   # вписать токен вместо PUT_BOT_TOKEN_HERE

# служба
sudo install -m 644 deploy/wordropbot.service /etc/systemd/system/wordropbot.service
sudo systemctl daemon-reload
sudo systemctl enable --now wordropbot
```

База лежит в `/var/lib/wordropbot/`, каталог создаёт systemd (`StateDirectory`).

## Журнал

```sh
journalctl -u wordropbot -f
```

## Обновление

```sh
cargo build --release
sudo install -m 755 target/release/wordropbot /usr/local/bin/wordropbot
sudo systemctl restart wordropbot
```

## Команды бота

- `/start`, `/help` - справка
- `/collection` - список коллекций; `/collection new <название>` - создать; `/collection <код>` - подключиться по коду
- `/words` - слова коллекции; `/remove <слово>` - удалить слово
- `/period <период>` - период квизов, например `1d` или `30m`
- `/pause`, `/resume` - приостановить и возобновить квизы
- `/quiz` - квиз сейчас; `/skip` - пропустить текущий
- `/stats` - статистика
