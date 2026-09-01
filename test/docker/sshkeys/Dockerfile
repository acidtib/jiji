FROM alpine:3.20

RUN apk add --no-cache openssh-keygen

WORKDIR /work
COPY boot.sh .
RUN chmod +x boot.sh

VOLUME /shared

HEALTHCHECK --interval=1s CMD test -f /shared/ssh/id_ed25519.pub

CMD ["./boot.sh"]
