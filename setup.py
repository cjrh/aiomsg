"""A setuptools based setup module.
See:
https://packaging.python.org/en/latest/distributing.html
https://github.com/pypa/sampleproject
"""

from setuptools import setup, find_packages
from os import path

here = path.abspath(path.dirname(__file__))
with open(path.join(here, 'README.rst'), encoding='utf-8') as f:
    long_description = f.read()

setup(
    name='aiosmartsock',
    version='2018.6.3',
    description='An ode to ØMQ',
    long_description=long_description,
    long_description_content_type='text/x-rst',
    url='https://github.com/cjrh/aiosmartsock',
    author='Caleb Hattingh',
    author_email='caleb.hattingh@gmail.com',
    classifiers=[
        'Development Status :: 3 - Alpha',
        'Intended Audience :: Developers',
        'Topic :: Software Development :: Build Tools',
        'License :: OSI Approved :: MIT License',
        'Programming Language :: Python :: 3.6',
    ],
    keywords='asyncio socket network',
    packages=find_packages(exclude=['contrib', 'docs', 'tests']),
    extras_require={
        'dev': ['check-manifest'],
        'test': ['pytest', 'pytest-cov', 'portpicker', 'pytest-benchmark'],
    },
)
