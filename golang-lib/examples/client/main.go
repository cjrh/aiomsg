// The connect end of the two-process demo: prints whatever the server sends,
// reconnecting automatically if the server restarts.
package main

import (
	"fmt"

	aiomsg "github.com/cjrh/aiomsg/golang-lib"
)

func main() {
	sock := aiomsg.NewSocket()
	defer sock.Close()

	if err := sock.Connect("127.0.0.1:25000"); err != nil {
		panic(err)
	}
	for msg := range sock.Messages() {
		fmt.Println(string(msg.Data))
	}
}
