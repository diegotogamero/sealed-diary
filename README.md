# Sealed Diary

Diario personal cifrado de extremo a extremo, multiplataforma y sin servidor central.
Texto, fotos, canciones y vídeos. Tus datos viven en infraestructura que montas tú.

> **Estado: en desarrollo.** Hay aplicación de escritorio con sincronización contra tu propio servidor,
> y una versión Android que compila pero aún no se ha probado en un dispositivo. No lo uses aún para datos reales:
> el código no ha pasado ninguna auditoría externa.

## Principios

- **Cero conocimiento.** Todo se cifra en tu dispositivo antes de tocar disco o red. El almacenamiento solo ve ficheros opacos con nombres aleatorios.
- **Sin cuentas.** No hay registro, email ni teléfono. No existe servidor del proyecto.
- **Sin telemetría.** La aplicación no contacta con nadie salvo con el almacenamiento que tú configures.
- **Contraseña siempre.** Cada apertura de la aplicación exige tu contraseña. Sin ella (o sin la clave de recuperación) los datos son irrecuperables, también para ti.

## Sincronización

No hay servidor del proyecto: lo montas tú. Guía paso a paso en [docs/servidor.md](docs/servidor.md).

## Modelo de amenaza

Protege frente a:

- Robo o pérdida de un dispositivo con la aplicación bloqueada.
- Servidor de sincronización comprometido, curioso o malicioso: no puede leer, y cualquier modificación, truncado, reordenación o intercambio de ficheros se detecta.
- Intercepción del tráfico de sincronización.

No protege frente a:

- Malware o keylogger en un dispositivo con el diario desbloqueado.
- Contraseñas débiles o reutilizadas.
- Alguien que te obligue a desbloquearlo.

Límite conocido: quien controle el almacenamiento puede borrar ficheros o devolver la bóveda entera a un estado anterior. Eso no revela nada, pero no se puede detectar sin una segunda copia. Haz copias de seguridad.

Qué ve quien controle el almacenamiento: número de objetos, tamaño aproximado de cada uno, cuándo se crean y desde qué IP se accede. Nunca contenido, títulos, fechas de las entradas ni tipo de fichero.

## Arquitectura

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

Solo se usan primitivas estándar de las bibliotecas [RustCrypto](https://github.com/RustCrypto). El formato exacto está documentado en `crates/sealed-diary-core/src/object.rs` y `keys.rs`.

## Hoja de ruta

1. [x] Núcleo: jerarquía de claves, formato de objetos, bóveda en disco.
2. [x] Modelo de entradas (texto, fecha, etiquetas, adjuntos) con historial de revisiones y detección de conflictos.
3. [x] Aplicación de escritorio (Tauri 2): bloqueo, editor, adjuntos con reproducción cifrada.
4. [x] Sincronización con WebDAV o con una carpeta, con resolución de conflictos.
5. [ ] Aplicación móvil: Android compila, falta probarla en dispositivo; iOS pendiente.
6. [x] [Tutorial de infraestructura propia](docs/servidor.md): servidor WebDAV con Docker, accesible por VPN.

## Estructura

- `crates/sealed-diary-core`: formato de la bóveda y criptografía. Sin interfaz, sin red.
- `crates/sealed-diary-sync`: sincronización con WebDAV o una carpeta. Decide en el dispositivo, con la clave, qué se copia y qué se borra; el servidor no decide nada.
- `server/`: servidor WebDAV mínimo para alojar tú mismo.
- `app/src-tauri`: aplicación Tauri. Expone órdenes a la interfaz y sirve los adjuntos descifrando al vuelo solo el tramo pedido.
- `app/ui`: interfaz en HTML, CSS y JavaScript sin dependencias ni empaquetador.

## Desarrollo

Requisitos: [Rust](https://rustup.rs), [pnpm](https://pnpm.io) y las [dependencias de sistema de Tauri](https://tauri.app/start/prerequisites/).

```sh
cargo test              # tests del núcleo y de la aplicación

cd app
pnpm install
pnpm tauri dev          # abre la aplicación
pnpm tauri build        # genera instaladores
```

### Android

Además de lo anterior: JDK 17 o posterior, Android SDK con NDK, y Rust instalado con [rustup](https://rustup.rs) con los destinos de Android.

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android

export JAVA_HOME=/ruta/al/jdk
export ANDROID_HOME=/ruta/al/Android/Sdk
export NDK_HOME=$ANDROID_HOME/ndk/<versión>

cd app
pnpm tauri android build --debug --apk --target aarch64
```

El APK queda en `app/src-tauri/gen/android/app/build/outputs/apk/universal/debug/`. La aplicación Android solo pide permiso de red, desactiva las copias de seguridad en la nube del sistema y bloquea las capturas de pantalla.

La bóveda se guarda en la carpeta de datos de la aplicación. Para usar otra carpeta, por ejemplo una sincronizada con Syncthing, define `SEALED_DIARY_VAULT=/ruta/a/la/carpeta` al arrancar.

## Licencia

[MIT](LICENSE). Puedes usar, copiar, modificar y redistribuir el código sin más condición que conservar el aviso de copyright.

Todas las dependencias son software libre. Casi todas tienen licencias permisivas (MIT, Apache-2.0, BSD o equivalentes). Las excepciones llegan a través de Tauri: cinco bibliotecas bajo MPL-2.0 (`cssparser`, `selectors` y auxiliares) y, en Linux, el motor web del sistema WebKitGTK bajo LGPL. Ninguna condiciona la licencia de este proyecto. No se aceptan dependencias de código cerrado.
