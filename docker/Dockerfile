FROM python:3.7.2-alpine3.9

RUN apk add --update openssl && \
    rm -rf /var/cache/apk/*

COPY . /aiomsg

RUN pip install -e aiomsg[all]
WORKDIR /aiomsg
CMD ["python", "-m", "pytest", "--cov", "aiomsg", "--cov-report", "term-missing"]
