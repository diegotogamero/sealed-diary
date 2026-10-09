# Lacre

Diario personal cifrado de extremo a extremo, multiplataforma y sin servidor central.
Texto, fotos, canciones y vídeos. Tus datos viven en infraestructura que montas tú.

> **Estado: en desarrollo.** Hoy existe el núcleo criptográfico (`crates/lacre-core`) con sus tests.
> Todavía no hay aplicación ni sincronización. No lo uses aún para datos reales:
> el código no ha pasado ninguna auditoría externa.

## Principios

- **Cero conocimiento.** Todo se cifra en tu dispositivo antes de tocar disco o red. El almacenamiento solo ve ficheros opacos con nombres aleatorios.
- **Sin cuentas.** No hay registro, email ni teléfono. No existe servidor del proyecto.
- **Sin telemetría.** La aplicación no contacta con nadie salvo con el almacenamiento que tú configures.
- **Contraseña siempre.** Cada apertura de la aplicación exige tu contraseña. Sin ella (o sin la clave de recuperación) los datos son irrecuperables, también para ti.

## Modelo de amenaza

Protege frente a:

- Robo o pérdida de un dispositivo con la aplicación bloqueada.
- Servidor de sincronización comprometido, curioso o malicioso: no puede leer, y cualquier modificación, truncado, reordenación o intercambio de ficheros se detecta.
- Intercepción del tráfico de sincronización.

No protege frente a:

- Malware o keylogger en un dispositivo con el diario desbloqueado.
- Contraseñas débiles o reutilizadas.
- Alguien que te obligue a desbloquearlo.

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
- **Bóveda:** una carpeta con `vault.json` (sin secretos) y `objects/` (blobs cifrados). Esa carpeta es lo único que se sincroniza.

Solo se usan primitivas estándar de las bibliotecas [RustCrypto](https://github.com/RustCrypto). El formato exacto está documentado en `crates/lacre-core/src/object.rs` y `keys.rs`.

## Hoja de ruta

1. [x] Núcleo: jerarquía de claves, formato de objetos, bóveda en disco.
2. [ ] Modelo de entradas (texto, fecha, etiquetas, adjuntos) e índice cifrado.
3. [ ] Aplicación de escritorio (Tauri 2): bloqueo, editor, galería, reproductor.
4. [ ] Sincronización: carpeta local (Syncthing) y WebDAV, con resolución de conflictos.
5. [ ] Aplicación móvil (Android, después iOS).
6. [ ] Tutorial de infraestructura propia: servidor WebDAV con Docker, accesible solo por VPN.

## Desarrollo

```sh
cargo test
```

## Licencia

[MIT](LICENSE). Puedes usar, copiar, modificar y redistribuir el código sin más condición que conservar el aviso de copyright.

Todas las dependencias tienen licencias permisivas (MIT, Apache-2.0, BSD o equivalentes). No se aceptan dependencias con licencias restrictivas o de código cerrado.
