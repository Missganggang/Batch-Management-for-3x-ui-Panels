# 在 Linux 服务器上部署 Nexus

Nexus 在服务器上运行 Rust 服务，管理员用浏览器登录。下面的命令在服务器的 Linux 终端中执行。使用 SSH 转发访问时，在自己的电脑上启动 SSH 并在使用网页期间保持连接。

## 1. 下载项目并设置密码

服务器需要 Docker Engine 和 Docker Compose v2。如果已有完整部署包 `dist/nexus-web.zip`，将它上传到服务器，执行 `unzip nexus-web.zip` 和 `cd nexus-web`。也可以用 Git 下载项目。

```sh
git clone https://github.com/Missganggang/Batch-Management-for-3x-ui-Panels.git nexus-web
cd nexus-web
```

在解压或克隆的项目目录中检查 Docker 和服务器架构。

```sh
docker version
docker compose version
uname -m
```

在项目目录创建 `.env`，设置一个专用于管理网页的长密码。账号固定为 `admin`。已有部署可以用同一命令打开原配置。

```sh
umask 077
touch .env
chmod 600 .env
nano .env
```

把下面的示例值换成自己的密码后保存。面板的 API 令牌稍后在网页里填写。

```dotenv
NEXUS_ADMIN_PASSWORD='请换成自己的长密码'
NEXUS_PANEL_DELETE_PASSWORD=''
```

`NEXUS_PANEL_DELETE_PASSWORD` 是删除面板时的确认密码，可以留空，留空时使用管理网页登录密码。升级时若要沿用原来的删除密码，请在服务器自己的 `.env` 中填入原值。

## 2. 启动服务

**Intel / AMD 64 位服务器**（`uname -m` 显示 `x86_64`）可以直接加载仓库中的预构建镜像。

```sh
docker load -i dist/nexus-web-linux-amd64.tar.gz
docker compose up -d --no-build --pull never
```

**ARM64 服务器**（`aarch64`）或需要从源码安装时，在项目目录执行下列命令。Docker 会下载构建所需的基础镜像和 Rust 依赖，并为本机架构编译。

```sh
docker compose up -d --build
```

检查服务状态。默认访问端口是 `8787`。最后一条命令会提示输入管理密码，正常时返回 `200`。

```sh
docker compose ps
docker compose logs --tail=50 nexus
curl --noproxy '*' -sS -o /dev/null -w '%{http_code}\n' -u admin http://127.0.0.1:8787/
```

## 3. 用浏览器登录

**SSH 转发**适合已有 SSH 账号的管理员。在自己的电脑上运行下列命令，替换服务器用户名和主机名，保持连接，然后打开 [管理网页](http://127.0.0.1:8787)。

```sh
ssh -N -L 127.0.0.1:8787:127.0.0.1:8787 user@server
```

**HTTPS 域名**适合从多台设备访问。将管理域名的 DNS 指向 Nexus 服务器，放行该服务器的 TCP 80、443。在宿主机上已安装的 Caddy 的 `/etc/caddy/Caddyfile` 中追加站点，用自己的域名替换示例域名。

```caddyfile
nexus.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

```sh
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

验证成功并重载后，在浏览器打开自己的 HTTPS 域名。采用其他反向代理时，保留浏览器请求的原始 `Host` 和 `Authorization` 头。

## 4. 接入 3x-ui 和开通第一条线路

1. 点击 **接入面板**。填写面板名称、从 Nexus 容器可访问的 3x-ui 3.4.2 面板 URL（含端口和自定义基础路径），以及该面板签发的 API 令牌。面板使用私网管理地址时，另填客户端可以访问的分享域名或 IP。
2. 打开 **新建线路**，选择目标面板，填写入站名称和 SOCKS5 地址。可选快捷期限，或按浏览器本地时区输入精确到秒的到期时间，例如 `2027-04-22 12:36:10`。流量上限按 GB 填写，两项都留空即永久有效且不限流量。创建后可以复制 VLESS 链接、复制带线路名称的二维码或下载 PNG。
3. 在 **我的线路** 查看在线状态、上下行流量和到期时间。批量管理、延期、导出和原有线路同步的操作见[使用说明](README.md)。

| 用途 | 连接方向 | 端口与配置 |
| --- | --- | --- |
| 管理网页 | 浏览器 → Nexus | 默认仅发布到宿主机 `127.0.0.1:8787`。若被占用，在 `.env` 设置 `NEXUS_PORT=8788`，重建容器并更新 Caddy 或 SSH 转发的目标端口。 |
| 3x-ui 管理 API | Nexus 容器 → 3x-ui | 使用接入面板时所填 URL 的端口。该地址要能从容器解析和访问。 |
| VLESS + TCP + Reality | 客户端 → 运行 3x-ui/Xray 的主机 | 默认在 `20000–20999/TCP` 中分配端口。在该主机的安全组和防火墙放行分配的端口，或放行规划的范围。 |
| SOCKS5 出口与 Reality 目标 | 运行 Xray 的主机 → 目标服务器 | 使用所填 SOCKS5 的地址和端口。Reality 默认检测 `www.amd.com:443`。 |

按需要在 `.env` 添加 `NEXUS_LINE_PORT_RANGE=20000-20999` 或 `NEXUS_REALITY_TARGET=www.amd.com:443` 并替换取值。修改后在部署目录执行 `docker compose up -d --no-build --pull never`。端口范围适用于之后新建的线路。

## 5. 备份

**迁移面板接入信息**时，在 **面板管理 → 备份数据** 设置备份密码，下载加密的 JSON 文件。文件包含面板名称、地址、地区、服务器归属、分享地址、备注和 API 令牌。新服务器部署同版本或兼容版本的管理台后，打开 **面板管理 → 恢复数据**，上传文件并输入备份时设置的密码。恢复会合并接入记录并跳过重复项。然后点击各面板的 **同步** 读取已有线路。若还需要原管理台的本地线路记录、操作历史和环境配置，请使用下方的完整备份步骤。

**3x-ui 面板数据**在网页的 **数据备份** 中下载。单独下载某台面板会得到 SQLite `.db` 或 PostgreSQL `.dump`。**一键备份全部** 会下载 `汇总YYYY-MM-DD.zip`，日期以浏览器当地日期为准。解压后可通过 `manifest.json` 核对各面板的数据库文件及备份结果。

**Nexus 管理台数据**包括本地线路登记、面板令牌和操作记录，位于容器的 `/var/lib/nexus`。下面的命令把数据、配置、当前实际运行的镜像备份到用户主目录的一个新目录。运行前确认管理台正在运行，备份目录有足够空间保存镜像。归档期间网页会短暂停止，现有 Xray 线路仍在各自的面板服务器上运行。

```sh
(
  set -eu
  umask 077
  stamp=$(date +%Y%m%d-%H%M%S)
  backup_dir="$HOME/nexus-backup-$stamp"
  mkdir "$backup_dir"
  cp .env compose.yaml "$backup_dir/"
  container=$(docker compose ps -q nexus)
  image=$(docker inspect --format '{{.Image}}' "$container")
  old_tag="nexus-web:backup-$stamp"
  docker image tag "$image" "$old_tag"
  docker image save -o "$backup_dir/image.tar" "$old_tag"
  printf '%s\n' "$old_tag" > "$backup_dir/image-tag"
  trap 'docker compose start nexus' EXIT
  docker compose stop nexus
  docker run --rm --volumes-from "$container:ro" --entrypoint tar "$image" \
    -C /var/lib/nexus -czf - . > "$backup_dir/nexus-data.tar.gz"
  tar -tzf "$backup_dir/nexus-data.tar.gz" ./nexus.sqlite
  printf '备份目录 %s\n' "$backup_dir"
)
```

保存好这个目录。它包含管理密码和可用于访问 3x-ui 的面板令牌，异机保存时请使用受限访问的存储。自定义的 Caddy 配置可另行备份。

## 6. 升级与回滚

**升级**前先完成上节的备份。进入原部署目录，沿用原来的 Compose 项目名，以便复用数据卷。使用 Git 安装的项目先执行 `git pull --ff-only`。使用 ZIP 安装时，将新版 ZIP 解压到临时目录，将其中的程序文件和镜像覆盖到原部署目录，保留原目录中的 `.env`。预构建安装接着执行以下命令。

```sh
docker load -i dist/nexus-web-linux-amd64.tar.gz
docker compose up -d --no-build --pull never
docker compose ps
```

源码安装在拉取更新后执行 `docker compose up -d --build`。登录管理网页，核对已有面板、线路和流量刷新。不要在升级时执行 `docker compose down -v`，该命令会删除项目数据卷。

**回滚到备份时刻**时，先保存升级后需要保留的变更。以下命令在原部署目录运行，用旧镜像和旧数据创建独立的恢复项目。将第一行路径换成实际备份目录。命令会在新数据卷中还原管理台，原数据卷可供后续核对。

```sh
(
  set -eu
  backup_dir="$HOME/nexus-backup-替换为备份时间"
  umask 077
  tar -tzf "$backup_dir/nexus-data.tar.gz" ./nexus.sqlite
  docker image load -i "$backup_dir/image.tar"
  old_tag=$(cat "$backup_dir/image-tag")
  recovery="nexus-restore-$(date +%Y%m%d-%H%M%S)"
  recovery_dir="$HOME/$recovery"
  mkdir "$recovery_dir"
  cp "$backup_dir/.env" "$backup_dir/compose.yaml" "$recovery_dir/"
  cat > "$recovery_dir/compose.override.yaml" <<EOF
name: $recovery
services:
  nexus:
    image: $old_tag
volumes:
  nexus-data:
    name: ${recovery}-data
    external: true
EOF
  docker volume create "${recovery}-data"
  docker run --rm -i --user 0:0 -v "${recovery}-data:/restore" \
    --entrypoint tar "$old_tag" -C /restore -xzf - < "$backup_dir/nexus-data.tar.gz"
  docker compose stop nexus
  if (cd "$recovery_dir" && docker compose -p "$recovery" up -d --no-build --pull never); then
    printf '恢复项目目录 %s\n' "$recovery_dir"
  else
    (cd "$recovery_dir" && docker compose -p "$recovery" stop nexus) || true
    docker compose start nexus
    exit 1
  fi
)
```

在恢复项目目录执行 `docker compose ps` 和 `docker compose logs --tail=50 nexus`，重新登录原网页地址核对备份时的面板与线路。以后从恢复项目目录管理该实例。如果使用过自定义 `COMPOSE_PROJECT_NAME`，请一直用上面生成的恢复项目名执行 `docker compose -p 项目名 …`。要切回升级后的服务，先在恢复项目目录执行 `docker compose stop nexus`，再回到原目录执行 `docker compose start nexus`。面板上的实际配置与管理台分别恢复，如需恢复 3x-ui 数据，使用对应面板的备份。

## 常见问题

| 现象 | 检查方法 |
| --- | --- |
| 启动时提示缺少 `NEXUS_ADMIN_PASSWORD` 或网页返回 401 | 确认当前目录的 `.env` 中已填写密码，重新执行 `docker compose up -d --no-build --pull never`。用账号 `admin` 和该密码登录。 |
| Caddy 返回 502 或浏览器打不开 | 先在服务器用第 2 步的 `curl` 检查本机服务，再检查 Caddy 上游端口和域名解析。SSH 转发需要保持连接。 |
| 添加面板时报 404、令牌错误或连接超时 | 核对 3x-ui URL 的自定义路径、管理端口、证书以及该面板的令牌。在容器里 `127.0.0.1` 指向 Nexus 容器自身。 |
| 可以登录，但线路连接超时 | 核对分享链接中的主机、端口是否从客户端可达，再检查 3x-ui 主机的安全组和防火墙。在 **我的线路 → 检测与修复** 可检查 Reality 目标和 SOCKS5 出口。 |
| 升级后看不到原有面板 | 核对使用的部署目录和 Compose 项目名，可用 `docker volume ls` 查看原数据卷。重新连接原卷或恢复备份后再操作。 |
| ARM 机器提示 `exec format error` | 在服务器的完整项目目录执行第 2 步的源码构建命令。 |
