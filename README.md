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

## Установка на сервер

Бот работает под отдельным пользователем как пользовательская служба systemd, так же как dancecrm на `cyprusswing.dance`. Один раз на сервере, от root:

```sh
useradd --create-home wordropbot
loginctl enable-linger wordropbot
install -d -o wordropbot -g wordropbot ~wordropbot/bin ~wordropbot/bot ~wordropbot/.config/systemd/user

# конфиг с токеном
install -o wordropbot -g wordropbot -m 600 config.example.toml ~wordropbot/bot/config.toml
editor ~wordropbot/bot/config.toml   # вписать токен вместо PUT_BOT_TOKEN_HERE

# служба
install -o wordropbot -g wordropbot -m 644 wordropbot.service ~wordropbot/.config/systemd/user/
systemctl --user -M wordropbot@ daemon-reload
systemctl --user -M wordropbot@ enable wordropbot
```

`config.example.toml` и `wordropbot.service` берутся из `deploy/`. База создаётся в `~wordropbot/bot/wordropbot.db`.

## Деплой

```sh
deploy/deploy.sh
```

Скрипт собирает бинарник в контейнере `rust:1-bookworm`, чтобы он работал со старой glibc сервера, копирует его в `~wordropbot/bin/` и перезапускает службу. Сервер и пользователь задаются переменными `DEPLOY_HOST` и `DEPLOY_USER`.

## Журнал

```sh
journalctl _SYSTEMD_USER_UNIT=wordropbot.service -f
```

## Команды бота

- `/start`, `/help` - справка
- `/collection` - список коллекций; `/collection new <название>` - создать; `/collection <код>` - подключиться по коду
- `/words` - слова коллекции; `/remove <слово>` - удалить слово
- `/period <период>` - период квизов, например `1d` или `30m`
- `/pause`, `/resume` - приостановить и возобновить квизы
- `/quiz` - квиз сейчас; `/skip` - пропустить текущий
- `/stats` - статистика
