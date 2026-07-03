package main

type Server struct {
	Address string
}

func (s *Server) Label() string {
	return s.Address
}