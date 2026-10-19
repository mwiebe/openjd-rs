# Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
# Copyright by contributors to this project.
# SPDX-License-Identifier: (Apache-2.0 OR MIT)
"""Deterministic boto3 stub for the CONTROL_FLOW golden tests.

`client("s3")` is backed by a directory: object `s3://B/K` is the file
`$OPENJD_GOLDEN_S3/B/K`. Only the calls the RFC samples make are provided.
Writes print a marker line, and `put_object` prints the object's body, so
the test output shows what a sample stored.
"""
import os
import shutil

ROOT = os.environ["OPENJD_GOLDEN_S3"]


def _path(bucket, key):
    return os.path.join(ROOT, bucket, key)


class _ListObjectsV2:
    def paginate(self, Bucket, Prefix=""):
        base = os.path.join(ROOT, Bucket)
        keys = sorted(
            os.path.relpath(os.path.join(d, n), base).replace(os.sep, "/")
            for d, _, names in os.walk(base)
            for n in names
        )
        contents = [{"Key": k, "Size": os.path.getsize(_path(Bucket, k))} for k in keys if k.startswith(Prefix)]
        # Like S3, a page with no objects has no Contents entry.
        yield {"Contents": contents} if contents else {}


class _S3:
    def get_paginator(self, name):
        assert name == "list_objects_v2", name
        return _ListObjectsV2()

    def download_file(self, bucket, key, filename):
        shutil.copyfile(_path(bucket, key), filename)
        print(f"boto3 stub: downloaded s3://{bucket}/{key}")

    def upload_file(self, filename, bucket, key):
        os.makedirs(os.path.dirname(_path(bucket, key)), exist_ok=True)
        shutil.copyfile(filename, _path(bucket, key))
        print(f"boto3 stub: uploaded s3://{bucket}/{key}")

    def head_object(self, Bucket, Key):
        return {"ContentLength": os.path.getsize(_path(Bucket, Key))}

    def put_object(self, Bucket, Key, Body):
        os.makedirs(os.path.dirname(_path(Bucket, Key)), exist_ok=True)
        with open(_path(Bucket, Key), "w", encoding="utf-8") as f:
            f.write(Body)
        print(f"boto3 stub: put s3://{Bucket}/{Key}: {Body}")


def client(service):
    assert service == "s3", service
    return _S3()
