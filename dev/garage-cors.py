#!/usr/bin/env python3
"""Lets the dev web origins upload to and download from the dev bucket (S3 PutBucketCors).

Browsers send a CORS preflight before the presigned PUT of an attachment; without a rule the dev
storage refuses it and every upload from the web client fails. Garage's CLI has no CORS command,
so this signs the S3 request itself (SigV4, standard library only).

    dev/garage-cors.py BUCKET ACCESS_KEY SECRET_KEY [ENDPOINT]
"""
import base64
import datetime
import hashlib
import hmac
import sys
import urllib.request

# The dev web client (trunk serve) and the e2e servers (tests/e2e), as registered in Keycloak.
ORIGINS = ["http://localhost:8082", "http://127.0.0.1:8082", "http://127.0.0.1:8083"]
REGION = "garage"


def main() -> None:
    bucket, access_key, secret_key = sys.argv[1:4]
    endpoint = sys.argv[4] if len(sys.argv) > 4 else "http://127.0.0.1:3900"
    origins = "".join(f"<AllowedOrigin>{origin}</AllowedOrigin>" for origin in ORIGINS)
    body = (
        "<CORSConfiguration><CORSRule>"
        f"{origins}<AllowedMethod>GET</AllowedMethod><AllowedMethod>PUT</AllowedMethod>"
        "<AllowedHeader>*</AllowedHeader><ExposeHeader>ETag</ExposeHeader>"
        "<MaxAgeSeconds>600</MaxAgeSeconds></CORSRule></CORSConfiguration>"
    ).encode()
    host = endpoint.split("://", 1)[1]
    now = datetime.datetime.now(datetime.timezone.utc)
    amz_date, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
    payload_hash = hashlib.sha256(body).hexdigest()
    md5 = base64.b64encode(hashlib.md5(body).digest()).decode()
    headers = {
        "content-md5": md5,
        "host": host,
        "x-amz-content-sha256": payload_hash,
        "x-amz-date": amz_date,
    }
    signed = ";".join(sorted(headers))
    canonical = "\n".join(
        ["PUT", f"/{bucket}", "cors=", *(f"{k}:{headers[k]}" for k in sorted(headers)), "", signed, payload_hash]
    )
    scope = f"{day}/{REGION}/s3/aws4_request"
    to_sign = "\n".join(["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical.encode()).hexdigest()])
    key = f"AWS4{secret_key}".encode()
    for part in (day, REGION, "s3", "aws4_request"):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    signature = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    request = urllib.request.Request(f"{endpoint}/{bucket}?cors=", data=body, method="PUT")
    for name, value in headers.items():
        if name != "host":
            request.add_header(name, value)
    request.add_header(
        "Authorization",
        f"AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed}, Signature={signature}",
    )
    with urllib.request.urlopen(request) as response:
        print(f"CORS set on {bucket} for {', '.join(ORIGINS)} (HTTP {response.status})")


if __name__ == "__main__":
    main()
