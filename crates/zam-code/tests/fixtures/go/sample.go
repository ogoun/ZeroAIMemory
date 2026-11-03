// Эталон экстрактора: Go.
package store

import (
	"errors"
	"fmt"
	nethttp "net/http"
)

const MaxItems = 64

const (
	ModeRead  = 1
	modeWrite = 2
)

var ErrClosed = errors.New("closed")

type Store struct {
	path  string
	items map[string]Item
}

type Item struct {
	ID   int
	Name string
}

type Backend interface {
	Open(path string) error
	Close() error
	fmt.Stringer
}

type ID = string

type Handler func(w nethttp.ResponseWriter, r *nethttp.Request)

type Mode int

type List[T any] struct {
	items []T
}

func New(path string) *Store {
	return &Store{path: path}
}

func open(path string) (*Store, error) {
	s := New(path)
	if err := s.Open(path); err != nil {
		return nil, fmt.Errorf("open: %w", err)
	}
	return s, nil
}

func (s *Store) Open(path string) error {
	s.path = path
	return nil
}

func (s *Store) Close() error {
	return nil
}

func (s Store) String() string {
	return s.path
}

func (s *Store) load() {}

func (i Item) Valid() bool {
	return i.ID > 0
}

func (m Mode) String() string {
	return fmt.Sprint(int(m))
}

func (l *List[T]) Push(v T) {
	l.items = append(l.items, v)
}

func (l *List[T]) Len() int {
	return len(l.items)
}

func Map[T, U any](xs []T, f func(T) U) []U {
	out := make([]U, 0, len(xs))
	for _, x := range xs {
		out = append(out, f(x))
	}
	return out
}

func handle(w nethttp.ResponseWriter, r *nethttp.Request) {
	fn := func() {}
	fn()
}

type (
	Reader interface {
		Read(p []byte) (int, error)
	}
	Writer interface {
		Write(p []byte) (int, error)
	}
	pair struct {
		a, b int
	}
)

func (p pair) Sum() int {
	return p.a + p.b
}

func init() {}

func main() {
	_ = open
}

type Cache struct {
	Store
}

func (c *Cache) Get(key string) (Item, bool) {
	v, ok := c.items[key]
	return v, ok
}

func (c *Cache) Put(key string, v Item) {
	c.items[key] = v
}

func (c *Cache) Open(path string) error {
	return c.Store.Open(path)
}

func Version() string { return "1" }

type Option func(*Store)

func WithPath(p string) Option {
	return func(s *Store) { s.path = p }
}

func (s *Store) Apply(opts ...Option) {
	for _, o := range opts {
		o(s)
	}
}

func Must(err error) {
	if err != nil {
		panic(err)
	}
}
