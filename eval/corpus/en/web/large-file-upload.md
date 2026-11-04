# Uploading large files from the seller dashboard

Sellers upload product photos, videos and price lists from the dashboard. Files of up to 2 GB
are accepted, so uploads must survive slow and unstable connections.

## Upload flow

The dashboard first asks the API for an upload session and receives an upload id and a part
size. The file is then sent in parts with `PUT` requests to the storage gateway, and a final
call completes the upload.

## Progress

Progress is computed from the `progress` events of each part request and shown per file. The
user can leave the page; uploads in progress are listed in a tray and continue in the
background tab.

## Resuming

If a part fails, only that part is sent again. After a page reload the dashboard asks the API
which parts are already stored and continues from there.

## Validation

The browser checks the file type and size before the upload starts, but the server checks them
again; client checks only save the user from waiting for a rejected upload.
