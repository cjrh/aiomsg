// The bind end of the two-process demo: publishes the time once a second.
//
//	go run ./examples/server
//	go run ./examples/client   # in another terminal
package main

import (
	"fmt"
	"time"

	aiomsg "github.com/cjrh/aiomsg/golang-lib"
)

func main() {
	sock := aiomsg.NewSocket(aiomsg.WithSendMode(aiomsg.Publish))
	defer sock.Close()

	addr, err := sock.Bind("127.0.0.1:25000")
	if err != nil {
		panic(err)
	}
	fmt.Println("bound to", addr, "publishing the time...")

	for {
		_ = sock.Send([]byte(time.Now().Format(time.RFC3339)))
		time.Sleep(time.Second)
	}
}
