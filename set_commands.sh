#!/bin/bash

cat <<EOF | curl --json @- "https://api.telegram.org/bot${1}/setMyCommands" | jq .
{
  "commands": [
    {
      "command": "info",
      "description": "Command device to report current status"
    },
    {
      "command": "version",
      "description": "Query bot version"
    }
  ]
}
EOF
