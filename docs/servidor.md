# Tu propio servidor de sincronización

Sealed Diary no tiene servidor central. Para ver el diario en varios dispositivos montas tú el almacenamiento, y la aplicación lo trata como un disco tonto y no fiable: solo recibe ficheros cifrados, y todo lo que devuelve se verifica antes de aceptarlo.

Cualquier servidor WebDAV sirve. Esta guía monta uno mínimo con Docker en unos minutos.

## Qué necesitas

- Un ordenador que esté encendido cuando quieras sincronizar: un PC viejo, una Raspberry Pi, un NAS o un VPS.
- [Docker](https://docs.docker.com/engine/install/) con el complemento Compose.

## 1. Arrancar el servidor

Copia la carpeta [`server/`](../server) de este repositorio a esa máquina y, dentro de ella:

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

## 2. Hacerlo accesible desde tus otros dispositivos

Con `DIARY_BIND=127.0.0.1` el servidor solo responde a la propia máquina. Elige una de estas opciones, cambia `DIARY_BIND` y repite `docker compose up -d`.

### Opción A: solo en casa

Pon en `DIARY_BIND` la IP de la máquina en tu red local, por ejemplo `192.168.1.20`. La dirección para la aplicación será `http://192.168.1.20:8080`. Sincronizarás cuando estés conectado a tu wifi.

### Opción B: desde cualquier sitio, por VPN (recomendada)

Una VPN entre tus dispositivos permite llegar al servidor desde fuera sin abrirlo a internet.

- [WireGuard](https://www.wireguard.com/): software libre de extremo a extremo; requiere configurar claves y abrir un puerto en el router.
- [Tailscale](https://tailscale.com/): mucho más fácil de instalar. Sus clientes son de código abierto, pero el servicio que coordina los dispositivos no lo es. [Headscale](https://headscale.net/) es la alternativa libre para alojarlo tú.

Pon en `DIARY_BIND` la IP que la VPN asigna al servidor (en Tailscale empieza por `100.`). La dirección será `http://100.x.y.z:8080`.

### Opción C: público con HTTPS

Solo si sabes mantener un servidor expuesto. Deja `DIARY_BIND=127.0.0.1` y pon delante un proxy inverso con certificado, por ejemplo [Caddy](https://caddyserver.com/):

```
diario.tudominio.org {
    reverse_proxy 127.0.0.1:8080
}
```

La dirección será `https://diario.tudominio.org`.

La aplicación rechaza direcciones `http://` que no sean de red privada o VPN, porque enviarían la contraseña del servidor sin cifrar.

## 3. Conectar el primer dispositivo

En la aplicación, con el diario desbloqueado: **Sincronización**, escribe la dirección, el usuario y la contraseña del servidor, y pulsa **Guardar y sincronizar**.

A partir de ahí sincroniza sola al abrir, al guardar y cada cinco minutos.

## 4. Añadir más dispositivos

En un dispositivo nuevo, en la pantalla inicial: **Ya tengo un diario en mi servidor**. Te pedirá los datos del servidor y la contraseña del diario.

## Copias de seguridad

Copia `server/data/` donde quieras: está cifrado. Hazlo de vez en cuando. La aplicación detecta ficheros manipulados, pero si alguien borra el servidor y además pierdes todos tus dispositivos, solo una copia te salva.

Guarda también, en papel y lejos de los dispositivos, la clave de recuperación que se mostró al crear el diario.

## Si editas en dos dispositivos a la vez

No se pierde nada. La entrada aparece marcada como conflicto, con las dos versiones disponibles; al guardar eliges cuál queda.

## Alternativas al servidor de esta guía

- **Nextcloud u otro WebDAV que ya tengas:** usa su dirección WebDAV apuntando a una carpeta vacía, por ejemplo `https://nube.tudominio.org/remote.php/dav/files/USUARIO/diario`.
- **Syncthing, sin servidor:** arranca la aplicación con `SEALED_DIARY_VAULT=/ruta/a/carpeta/sincronizada` y deja que Syncthing replique esa carpeta. No hace falta configurar nada en **Sincronización**.
