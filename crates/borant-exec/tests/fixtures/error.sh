#!/bin/sh
cat > /dev/null
echo "{\"error\": {\"code\": \"$1\", \"message\": \"marker-value\"}}"
