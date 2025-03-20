#!/bin/bash

curl "https://api.telegram.org/bot${1}/getWebhookInfo" | jq .
