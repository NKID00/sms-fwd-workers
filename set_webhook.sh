#!/bin/bash

cat <<EOF | curl --json @- "https://api.telegram.org/bot${1}/setWebhook" | jq .
{
  "url": "https://sms.nk0.uk/",
  "allowed_updates": [ "message" ],
  "drop_pending_updates": true,
  "secret_token": "${2}"
}
EOF
