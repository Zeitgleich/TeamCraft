#!/usr/bin/env python3
"""Sets the dev bucket's CORS rule and lifecycle rule (S3 PutBucketCors, PutBucketLifecycle).

Browsers send a CORS preflight before the presigned PUT of an attachment; without a rule the dev
storage refuses it and every upload from the web client fails. Uploads in parts read each part's
ETag, so the rule exposes it. The lifecycle rule aborts uploads in parts nobody finished (the
server aborts the ones it knows; this catches what a crash left behind). Garage's CLI has no
command for either, so this signs the S3 requests itself (SigV4, standard library only).

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
# Uploads in parts left unfinished this long are aborted by the storage itself.
ABORT_INCOMPLETE_AFTER_DAYS = 3


def main() -> None:
    bucket, access_key, secret_key = sys.argv[1:4]
    endpoint = sys.argv[4] if len(sys.argv) > 4 else "http://127.0.0.1:3900"
    origins = "".join(f"<AllowedOrigin>{origin}</AllowedOrigin>" for origin in ORIGINS)
    cors = (
        "<CORSConfiguration><CORSRule>"
        f"{origins}<AllowedMethod>GET</AllowedMethod><AllowedMethod>PUT</AllowedMethod>"
        "<AllowedHeader>*</AllowedHeader><ExposeHeader>ETag</ExposeHeader>"
        "<MaxAgeSeconds>600</MaxAgeSeconds></CORSRule></CORSConfiguration>"
    )
    status = put_subresource(endpoint, bucket, "cors", cors, access_key, secret_key)
    print(f"CORS set on {bucket} for {', '.join(ORIGINS)} (HTTP {status})")
    lifecycle = (
        "<LifecycleConfiguration><Rule><ID>abort-incomplete-uploads</ID><Status>Enabled</Status>"
        "<Filter></Filter><AbortIncompleteMultipartUpload>"
        f"<DaysAfterInitiation>{ABORT_INCOMPLETE_AFTER_DAYS}</DaysAfterInitiation>"
        "</AbortIncompleteMultipartUpload></Rule></LifecycleConfiguration>"
    )
    status = put_subresource(endpoint, bucket, "lifecycle", lifecycle, access_key, secret_key)
    print(f"lifecycle set on {bucket}: abort uploads in parts after {ABORT_INCOMPLETE_AFTER_DAYS} days (HTTP {status})")


def put_subresource(endpoint: str, bucket: str, name: str, text: str, access_key: str, secret_key: str) -> int:
    """PUTs a bucket subresource (`?cors`, `?lifecycle`) signed with SigV4; returns the status."""
    body = text.encode()
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
        ["PUT", f"/{bucket}", f"{name}=", *(f"{k}:{headers[k]}" for k in sorted(headers)), "", signed, payload_hash]
    )
    scope = f"{day}/{REGION}/s3/aws4_request"
    to_sign = "\n".join(["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical.encode()).hexdigest()])
    key = f"AWS4{secret_key}".encode()
    for part in (day, REGION, "s3", "aws4_request"):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    signature = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    request = urllib.request.Request(f"{endpoint}/{bucket}?{name}=", data=body, method="PUT")
    for header, value in headers.items():
        if header != "host":
            request.add_header(header, value)
    request.add_header(
        "Authorization",
        f"AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed}, Signature={signature}",
    )
    with urllib.request.urlopen(request) as response:
        return response.status


if __name__ == "__main__":
    main()
