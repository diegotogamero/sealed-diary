# Sealed Diary

Diario personal cifrado de extremo a extremo, multiplataforma y sin servidor central.
Texto, fotos, canciones y vídeos. Tus datos viven en infraestructura que montas tú.

> **Estado: en desarrollo.** La aplicación de escritorio funciona en Linux y sincroniza con tu propio servidor.
> La versión Android compila, pero aún no se ha probado en un dispositivo. Windows y macOS no se han probado.
> El código no ha pasado ninguna auditoría externa: no guardes todavía nada que te duela perder.

## Índice

1. [Principios](#principios)
2. [Instalar en el ordenador](#instalar-en-el-ordenador)
3. [Usar el diario](#usar-el-diario)
4. [Sincronizar entre dispositivos](#sincronizar-entre-dispositivos)
5. [Android](#android)
6. [Copias de seguridad](#copias-de-seguridad)
7. [Problemas frecuentes](#problemas-frecuentes)
8. [Seguridad](#seguridad)
9. [Desarrollo](#desarrollo)
10. [Hoja de ruta](#hoja-de-ruta)
11. [Licencia](#licencia)

## Principios

- **Cero conocimiento.** Todo se cifra en tu dispositivo antes de tocar disco o red. El almacenamiento solo ve ficheros opacos con nombres aleatorios.
- **Sin cuentas.** No hay registro, email ni teléfono. No existe servidor del proyecto.
- **Sin telemetría.** La aplicación no contacta con nadie salvo con el almacenamiento que tú configures.
- **Contraseña siempre.** Cada apertura de la aplicación exige tu contraseña. Sin ella (o sin la clave de recuperación) los datos son irrecuperables, también para ti.

## Instalar en el ordenador

Todavía no hay instaladores publicados: la aplicación se compila desde el código. Son tres pasos.

### 1. Requisitos

- [Rust](https://rustup.rs)
- [Node.js](https://nodejs.org) y [pnpm](https://pnpm.io/installation)
- Las bibliotecas de sistema que usa Tauri. Si alguna orden falla, la lista al día está en los [requisitos oficiales de Tauri](https://tauri.app/start/prerequisites/):

| Sistema | Orden |
| --- | --- |
| Arch, CachyOS, Manjaro | `sudo pacman -S --needed webkit2gtk-4.1 base-devel curl wget file openssl libayatana-appindicator librsvg` |
| Debian, Ubuntu | `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libssl-dev libayatana-appindicator3-dev librsvg2-dev` |
| Fedora | `sudo dnf install webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel gcc` |
| Windows, macOS | Sigue los [requisitos oficiales de Tauri](https://tauri.app/start/prerequisites/). |

### 2. Descargar el código

```sh
git clone https://github.com/diegotogamero/sealed-diary.git
cd sealed-diary/app
pnpm install
```

### 3. Abrir la aplicación

Para probarla directamente:

```sh
pnpm tauri dev
```

La primera compilación tarda unos minutos; las siguientes, segundos.

Para instalarla como una aplicación más:

```sh
pnpm tauri build
```

Los instaladores quedan en `target/release/bundle/`, en la raíz del repositorio:

| Fichero | Para |
| --- | --- |
| `appimage/Sealed Diary_0.1.0_amd64.AppImage` | Cualquier Linux. Dale permiso de ejecución y ábrelo; no necesita instalación. |
| `deb/Sealed Diary_0.1.0_amd64.deb` | Debian, Ubuntu: `sudo apt install ./"Sealed Diary_0.1.0_amd64.deb"` |
| `rpm/Sealed Diary-0.1.0-1.x86_64.rpm` | Fedora: `sudo dnf install ./"Sealed Diary-0.1.0-1.x86_64.rpm"` |

En Windows y macOS la misma orden genera sus propios instaladores.

## Usar el diario

### Crear el diario

1. Al abrir por primera vez, elige una contraseña de al menos 10 caracteres. Mejor una frase larga que recuerdes que una contraseña corta y rara.
2. La aplicación muestra tu **clave de recuperación**. Aparece una sola vez. Apúntala en papel y guárdala lejos del dispositivo: es la única forma de entrar si olvidas la contraseña.
3. Marca que la has guardado y entra.

Nadie puede restablecer tu contraseña. Si pierdes la contraseña y la clave de recuperación, el diario se pierde.

### Escribir

- **Nueva entrada** abre una entrada con la fecha de hoy. Puedes cambiar la fecha.
- Escribe título, texto y, si quieres, etiquetas separadas por comas.
- **Guardar** la cifra y la almacena. Nada se guarda solo mientras escribes, salvo cuando el diario se bloquea por inactividad.
- La lista de la izquierda muestra las entradas, la más reciente arriba.

### Fotos, audio y vídeo

**Adjuntar foto, audio o vídeo** abre el selector de ficheros. El fichero se cifra al momento y aparece en la entrada; pulsa **Guardar** para conservarlo.

Los adjuntos se ven y se reproducen dentro de la aplicación. Se descifra solo el tramo que se está viendo, y nunca se escribe una copia sin cifrar en el disco.

**Quitar** elimina un adjunto de la entrada. El fichero cifrado se borra al guardar.

### Bloquear

- **Bloquear** cierra el diario al instante.
- Se bloquea solo tras 5 minutos sin tocar el teclado ni el ratón, y en el móvil tras 2 minutos en segundo plano. Lo que estuvieras escribiendo se guarda antes.
- Al bloquear, la clave se borra de la memoria y la pantalla queda vacía.

### Si olvidas la contraseña

En la pantalla de desbloqueo: **He olvidado la contraseña**. Escribe la clave de recuperación y elige una contraseña nueva.

### Dónde están los datos

| Sistema | Carpeta |
| --- | --- |
| Linux | `~/.local/share/io.github.diegotogamero.sealeddiary/` |
| Windows | `%APPDATA%\io.github.diegotogamero.sealeddiary\` |
| macOS | `~/Library/Application Support/io.github.diegotogamero.sealeddiary/` |

Dentro, `vault/` contiene el diario cifrado. Para guardarlo en otra carpeta, arranca la aplicación con la variable `SEALED_DIARY_VAULT=/ruta/a/la/carpeta`.

## Sincronizar entre dispositivos

Sealed Diary no tiene servidor central. Para ver el diario en varios dispositivos montas tú el almacenamiento, y la aplicación lo trata como un disco tonto y no fiable: solo recibe ficheros cifrados, y todo lo que devuelve se verifica antes de aceptarlo.

Cualquier servidor WebDAV sirve. Estos pasos montan uno mínimo con Docker en unos minutos.

### Qué necesitas

- Un ordenador que esté encendido cuando quieras sincronizar: un PC viejo, una Raspberry Pi, un NAS o un VPS.
- [Docker](https://docs.docker.com/engine/install/) con el complemento Compose.

### 1. Arrancar el servidor

Copia la carpeta [`server/`](server) de este repositorio a esa máquina y, dentro de ella:

```sh
cp .env.example .env
```

Edita `.env`:

```sh
DIARY_USER=diario
DIARY_PASSWORD=una-contraseña-larga-y-distinta-a-la-del-diario
DIARY_BIND=127.0.0.1
```

Esta contraseña es la del servidor, no la del diario. Usa una diferente: quien la consiga podría borrar tus ficheros cifrados, aunque nunca leerlos.

Arranca:

```sh
docker compose up -d
```

Los ficheros cifrados quedan en `server/data/`.

### 2. Hacerlo accesible desde tus otros dispositivos

Con `DIARY_BIND=127.0.0.1` el servidor solo responde a la propia máquina. Elige una opción, cambia `DIARY_BIND` y repite `docker compose up -d`.

**Opción A: solo en casa.** Pon en `DIARY_BIND` la IP de la máquina en tu red local, por ejemplo `192.168.1.20`. La dirección para la aplicación será `http://192.168.1.20:8080`. Sincronizarás cuando estés conectado a tu wifi.

**Opción B: desde cualquier sitio, por VPN (recomendada).** Una VPN entre tus dispositivos permite llegar al servidor desde fuera sin abrirlo a internet.

- [WireGuard](https://www.wireguard.com/): software libre de extremo a extremo; requiere configurar claves y abrir un puerto en el router.
- [Tailscale](https://tailscale.com/): mucho más fácil de instalar. Sus clientes son de código abierto, pero el servicio que coordina los dispositivos no lo es. [Headscale](https://headscale.net/) es la alternativa libre para alojarlo tú.

Pon en `DIARY_BIND` la IP que la VPN asigna al servidor (en Tailscale empieza por `100.`). La dirección será `http://100.x.y.z:8080`.

**Opción C: público con HTTPS.** Solo si sabes mantener un servidor expuesto. Deja `DIARY_BIND=127.0.0.1` y pon delante un proxy inverso con certificado, por ejemplo [Caddy](https://caddyserver.com/):

```
diario.tudominio.org {
    reverse_proxy 127.0.0.1:8080
}
```

La dirección será `https://diario.tudominio.org`.

La aplicación rechaza direcciones `http://` que no sean de red privada o VPN, porque enviarían la contraseña del servidor sin cifrar.

### 3. Conectar el primer dispositivo

Con el diario desbloqueado: **Sincronización**, escribe la dirección, el usuario y la contraseña del servidor, y pulsa **Guardar y sincronizar**.

A partir de ahí sincroniza sola al abrir, al guardar y cada cinco minutos. El estado aparece bajo el botón. La contraseña del servidor se guarda cifrada con la clave del diario.

### 4. Añadir más dispositivos

En un dispositivo nuevo, en la pantalla inicial: **Ya tengo un diario en mi servidor**. Te pedirá los datos del servidor y la contraseña del diario.

### Si editas en dos dispositivos a la vez

No se pierde nada. La entrada aparece marcada como conflicto, con las dos versiones disponibles. Elige la que quieras ver, corrige lo que haga falta y guarda: lo que haya en pantalla sustituye a ambas.

### Si cambias la contraseña en un dispositivo

Los demás la adoptan en la siguiente sincronización. Por ahora la contraseña solo se cambia con la clave de recuperación, desde **He olvidado la contraseña**.

### Alternativas al servidor de esta guía

- **Nextcloud u otro WebDAV que ya tengas:** usa su dirección WebDAV apuntando a una carpeta vacía, por ejemplo `https://nube.tudominio.org/remote.php/dav/files/USUARIO/diario`.
- **Syncthing, sin servidor:** arranca la aplicación con `SEALED_DIARY_VAULT=/ruta/a/carpeta/sincronizada` y deja que Syncthing replique esa carpeta. No hace falta configurar nada en **Sincronización**.

## Android

> Compila y genera un APK, pero todavía no se ha probado en un dispositivo. Espera fallos.

### Compilar el APK

Además de los requisitos del ordenador: JDK 17 o posterior, Android SDK con NDK, y Rust instalado con rustup.

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android

export JAVA_HOME=/ruta/al/jdk
export ANDROID_HOME=/ruta/al/Android/Sdk
export NDK_HOME=$ANDROID_HOME/ndk/<versión>

cd app
pnpm tauri android build --debug --apk --target aarch64
```

El APK queda en `app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`.

### Instalarlo

Copia el APK al móvil y ábrelo (tendrás que permitir instalar aplicaciones de origen desconocido), o con cable y la depuración USB activada:

```sh
adb install app-universal-debug.apk
```

Sirve para móviles ARM de 64 bits con Android 7 o superior.

### Qué hace distinto en Android

- Solo pide permiso de red, para sincronizar.
- Desactiva las copias de seguridad en la nube del sistema.
- Bloquea las capturas de pantalla y la miniatura en la lista de aplicaciones recientes.

## Copias de seguridad

La aplicación detecta ficheros manipulados, pero no puede recuperar lo que se borra en todas partes. Ten al menos una copia.

- **Con servidor:** copia `server/data/` donde quieras. Está cifrado.
- **Sin servidor:** copia la carpeta `vault/` de los datos de la aplicación.
- **Siempre:** guarda en papel la clave de recuperación.

Para restaurar:

- **Copia de `server/data/`:** ponla como `data/` de un servidor nuevo, arráncalo y usa **Ya tengo un diario en mi servidor**.
- **Copia de `vault/`:** colócala en una carpeta y arranca la aplicación con `SEALED_DIARY_VAULT` apuntando a ella.

Las dos copias no son intercambiables: el servidor guarda los ficheros con otra disposición de carpetas.

## Problemas frecuentes

**La ventana no se abre en Linux con gráfica NVIDIA y Wayland.** La aplicación ya aplica la solución conocida (`WEBKIT_DISABLE_DMABUF_RENDERER=1`). Si aun así falla, prueba a arrancarla con `GDK_BACKEND=x11`.

**«Ese servidor contiene un diario distinto».** La dirección apunta a una carpeta donde ya hay otro diario. La aplicación se niega a sobrescribirlo. Usa una carpeta vacía o la dirección correcta.

**«Dirección no válida».** Has escrito `http://` con un nombre de dominio o una IP pública. Usa `https://`, o `http://` solo con una IP de tu red local o de tu VPN.

**«N entrada(s) no se pueden leer».** Hay ficheros dañados, manipulados o escritos por una versión más reciente de la aplicación. El resto del diario sigue funcionando. Actualiza la aplicación o restaura esos ficheros desde una copia.

**«N fichero(s) del servidor no superaron la verificación».** El servidor devolvió ficheros que no son tuyos o están a medio subir desde otro dispositivo. Se ignoran. Si el aviso no desaparece tras varias sincronizaciones, revisa quién tiene acceso al servidor.

**He olvidado la contraseña y no tengo la clave de recuperación.** No hay solución. Es la consecuencia de que nadie más pueda leer tu diario.

## Seguridad

### Modelo de amenaza

Protege frente a:

- Robo o pérdida de un dispositivo con la aplicación bloqueada.
- Servidor de sincronización comprometido, curioso o malicioso: no puede leer, y cualquier modificación, truncado, reordenación o intercambio de ficheros se detecta.
- Intercepción del tráfico de sincronización.

No protege frente a:

- Malware o keylogger en un dispositivo con el diario desbloqueado.
- Contraseñas débiles o reutilizadas.
- Alguien que te obligue a desbloquearlo.

Límite conocido: quien controle el almacenamiento puede borrar ficheros o devolver la bóveda entera a un estado anterior. Eso no revela nada, pero no se puede detectar sin una segunda copia.

Qué ve quien controle el almacenamiento: número de objetos, tamaño aproximado de cada uno, cuándo se crean y desde qué IP se accede. Nunca contenido, títulos, fechas de las entradas ni tipo de fichero.

### Arquitectura

```text
contraseña ──Argon2id──▶ KEK ──┐
                               ├──▶ clave maestra ──HKDF──▶ clave por objeto
clave de recuperación ─HKDF──▶ ┘
```

- **Derivación:** Argon2id (128 MiB, 3 pasadas por defecto). La contraseña se normaliza a NFKC para que funcione igual en todos los teclados.
- **Clave maestra:** aleatoria, de 256 bits, envuelta por la contraseña y por la clave de recuperación. Cambiar la contraseña no obliga a recifrar nada.
- **Objetos:** cada entrada o fichero multimedia se cifra con XChaCha20-Poly1305 en trozos de 64 KiB autenticados uno a uno. Esto permite reproducir un vídeo saltando a cualquier punto sin descifrarlo entero y con memoria constante.
- **Entradas:** nunca se editan en sitio. Cada guardado escribe una revisión nueva que nombra a las que sustituye. Si dos dispositivos editan la misma entrada sin conexión, al sincronizar aparecen ambas versiones como conflicto; ninguna pisa a la otra. Una revisión antigua restaurada por el servidor no vuelve a ser la actual.
- **Cabecera autenticada:** `vault.json` lleva un código calculado con la clave maestra. Un servidor no puede sustituirlo por uno propio, y un cambio de contraseña hecho en un dispositivo se acepta en los demás solo si es genuino.
- **Bóveda:** una carpeta con `vault.json` (sin secretos), `entries/` y `media/` (blobs cifrados con nombre aleatorio). Esa carpeta es lo único que se sincroniza.
- **Sincronización:** el dispositivo decide, con la clave, qué se copia y qué se borra. Un adjunto solo se elimina si un dispositivo lo quitó a propósito y ninguna entrada legible lo referencia; ante la duda, se copia.

Solo se usan primitivas estándar de las bibliotecas [RustCrypto](https://github.com/RustCrypto). El formato exacto está documentado en `crates/sealed-diary-core/src/object.rs` y `keys.rs`.

### Avisar de un fallo de seguridad

Abre una incidencia en el repositorio. Si el fallo permite leer diarios ajenos, no publiques los detalles: pide primero una vía privada de contacto en la propia incidencia.

## Desarrollo

### Estructura

- `crates/sealed-diary-core`: formato de la bóveda y criptografía. Sin interfaz, sin red.
- `crates/sealed-diary-sync`: sincronización con WebDAV o una carpeta.
- `app/src-tauri`: aplicación Tauri. Expone órdenes a la interfaz y sirve los adjuntos descifrando al vuelo solo el tramo pedido.
- `app/ui`: interfaz en HTML, CSS y JavaScript sin dependencias ni empaquetador.
- `server/`: servidor WebDAV mínimo para alojar tú mismo.

### Tests

```sh
cargo test
```

Dos tests necesitan un servidor WebDAV real con la carpeta vacía y no se ejecutan por defecto. Con el de `server/` arrancado:

```sh
export SEALED_DIARY_TEST_WEBDAV=http://127.0.0.1:8080
export SEALED_DIARY_TEST_USER=diario
export SEALED_DIARY_TEST_PASSWORD=la-del-fichero-.env
cargo test -- --ignored
```

Antes de enviar cambios:

```sh
cargo fmt
cargo clippy --all-targets
```

## Hoja de ruta

- [x] Núcleo: jerarquía de claves, formato de objetos, bóveda en disco.
- [x] Entradas con historial de revisiones y detección de conflictos.
- [x] Aplicación de escritorio: bloqueo, editor, adjuntos con reproducción cifrada.
- [x] Sincronización con WebDAV o con una carpeta.
- [x] Servidor propio con Docker.
- [ ] Android: probar en dispositivo y corregir.
- [ ] Descarga de adjuntos bajo demanda, para no llenar el móvil de vídeos.
- [ ] Búsqueda y filtro por etiquetas.
- [ ] Elegir la carpeta del diario y cambiar la contraseña desde la interfaz.
- [ ] Instaladores publicados.
- [ ] iOS.
- [ ] Auditoría de seguridad externa.

## Licencia

[MIT](LICENSE). Puedes usar, copiar, modificar y redistribuir el código sin más condición que conservar el aviso de copyright.

Todas las dependencias son software libre. Casi todas tienen licencias permisivas (MIT, Apache-2.0, BSD o equivalentes). Las excepciones llegan a través de Tauri: cinco bibliotecas bajo MPL-2.0 (`cssparser`, `selectors` y auxiliares) y, en Linux, el motor web del sistema WebKitGTK bajo LGPL. Ninguna condiciona la licencia de este proyecto. No se aceptan dependencias de código cerrado.
