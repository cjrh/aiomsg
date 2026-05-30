// Command conformance_agent is the Go test agent for the cross-language interop
// suite. It exposes the same CLI as the Python and Rust agents so the runner
// can pair any two languages in any role.
//
// A sink prints each received message (one per line) and exits after --count
// messages.
package main

import (
	"bufio"
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
	if *role == "bind" {
		if _, err := sock.Bind(addr); err != nil {
			fmt.Fprintln(os.Stderr, "bind failed:", err)
			os.Exit(1)
		}
	} else {
		if err := sock.Connect(addr); err != nil {
			fmt.Fprintln(os.Stderr, "connect failed:", err)
			os.Exit(1)
		}
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
