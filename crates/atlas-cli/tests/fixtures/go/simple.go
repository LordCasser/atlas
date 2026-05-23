package main

import "fmt"

// Server is a simple HTTP server.
type Server struct {
	Port int
}

// Start begins listening on the configured port.
func (s *Server) Start() {
	fmt.Printf("listening on :%d\n", s.Port)
}

func main() {
	srv := Server{Port: 8080}
	srv.Start()
}
