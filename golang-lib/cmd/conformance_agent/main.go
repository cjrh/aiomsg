// Command conformance_agent is the Go test agent for the cross-language interop
// suite. It exposes the same CLI as the Python and Rust agents so the runner
// can pair any two languages in any role.
//
// A sink prints each received message (one per line) and exits after --count
// messages.
package main

import (
	"bufio"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"flag"
	"fmt"
	"os"
	"time"

	aiomsg "github.com/cjrh/aiomsg/golang-lib"
)

func main() {
	role := flag.String("role", "connect", "bind | connect")
	host := flag.String("host", "127.0.0.1", "host")
	port := flag.Int("port", 25000, "port")
	sendMode := flag.String("send-mode", "roundrobin", "roundrobin | publish")
	behavior := flag.String("behavior", "sink", "source | sink | echo")
	count := flag.Int("count", 10, "messages to send/receive")
	prefix := flag.String("prefix", "m", "message prefix")
	delivery := flag.String("delivery", "at-most-once", "at-most-once | at-least-once")
	identityHex := flag.String("identity", "", "32 hex chars (16 bytes)")
	linger := flag.Float64("linger", 1.0, "seconds a source waits after sending")
	useTLS := flag.String("tls", "false", "wrap the transport in TLS")
	tlsCert := flag.String("tls-cert", "", "server cert PEM (bind side)")
	tlsKey := flag.String("tls-key", "", "server key PEM (bind side)")
	tlsCA := flag.String("tls-ca", "", "trusted CA PEM (connect side)")
	tlsServerName := flag.String("tls-server-name", "", "name to verify (connect side; default = host)")
	flag.Parse()

	var opts []aiomsg.Option
	if *sendMode == "publish" {
		opts = append(opts, aiomsg.WithSendMode(aiomsg.Publish))
	} else {
		opts = append(opts, aiomsg.WithSendMode(aiomsg.RoundRobin))
	}
	if *delivery == "at-least-once" {
		opts = append(opts, aiomsg.WithDeliveryGuarantee(aiomsg.AtLeastOnce))
	}
	if *identityHex != "" {
		b, err := hex.DecodeString(*identityHex)
		if err != nil {
			fmt.Fprintln(os.Stderr, "bad identity:", err)
			os.Exit(1)
		}
		var id aiomsg.Identity
		copy(id[:], b)
		opts = append(opts, aiomsg.WithIdentity(id))
	}

	sock := aiomsg.NewSocket(opts...)
	defer sock.Close()

	addr := fmt.Sprintf("%s:%d", *host, *port)
	tlsOn := *useTLS == "true"
	var err error
	switch {
	case *role == "bind" && tlsOn:
		_, err = sock.BindTLS(addr, serverTLSConfig(*tlsCert, *tlsKey))
	case *role == "bind":
		_, err = sock.Bind(addr)
	case tlsOn:
		name := *tlsServerName
		if name == "" {
			name = *host
		}
		err = sock.ConnectTLS(addr, clientTLSConfig(*tlsCA, name))
	default:
		err = sock.Connect(addr)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, *role, "failed:", err)
		os.Exit(1)
	}

	lingerDur := time.Duration(*linger * float64(time.Second))

	switch *behavior {
	case "source":
		for i := 0; i < *count; i++ {
			_ = sock.Send([]byte(fmt.Sprintf("%s%d", *prefix, i)))
		}
		time.Sleep(lingerDur)
	case "echo":
		for i := 0; i < *count; i++ {
			id, msg, ok := sock.RecvIdentity()
			if !ok {
				break
			}
			_ = sock.SendTo(id, msg)
		}
		time.Sleep(lingerDur)
	default: // sink
		w := bufio.NewWriter(os.Stdout)
		for received := 0; received < *count; received++ {
			msg, ok := sock.Recv()
			if !ok {
				break
			}
			w.Write(msg)
			w.WriteByte('\n')
			w.Flush()
		}
	}
}

func serverTLSConfig(certPath, keyPath string) *tls.Config {
	cert, err := tls.LoadX509KeyPair(certPath, keyPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "load cert:", err)
		os.Exit(1)
	}
	return &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS12}
}

func clientTLSConfig(caPath, serverName string) *tls.Config {
	pem, err := os.ReadFile(caPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "read CA:", err)
		os.Exit(1)
	}
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(pem) {
		fmt.Fprintln(os.Stderr, "no certs found in CA PEM")
		os.Exit(1)
	}
	return &tls.Config{RootCAs: pool, ServerName: serverName, MinVersion: tls.VersionTLS12}
}
